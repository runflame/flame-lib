# FlameVM Type Map

A dependency map of every public / internal type in the `flamevm` crate,
grouped by layer. Arrows read **"depends on / contains"**. External crates
(`bulletproofs`/`r1cs`, `curve25519-dalek`, `merlin`, `musig`, `spacesuit`,
`readerwriter`, `merkle`) are named but not expanded.

Diagrams and tables are space-aligned for a fixed-width text editor.
Authoritative semantics live in `spec.md` and the ADRs; this file is a
hand-maintained index — keep it in sync when types move.

---

## Layer overview

```
  +--------------------------------------------------------------------+
  |  execution  VM · CallFrame · CallKind · Delegate                   |
  |             Prover · Verifier · InternalDelegate · TxResult        |
  +--------------------------------------------------------------------+
                                     | drives
                                     v
  +--------------------------------------------------------------------+
  |  program    ScriptBuilder --build--> Script · Instruction          |
  +--------------------------------------------------------------------+
                                     | runs
                                     v
  +--------------------------------------------------------------------+
  |  value      Value -- the stack-element union (13 variants)         |
  +--------------------------------------------------------------------+
                                     | operates on
                                     v
  +--------------------------------------------------------------------+
  |  data       Int253 · String · Dict · Point · Merlin                |
  |  linear     Token · WideToken · ClearToken · Cell                  |
  |  cs         Variable · Expression · Constraint · Commitment · MSM  |
  +--------------------------------------------------------------------+
                                     | produced / committed by
                                     v
  +--------------------------------------------------------------------+
  |  effects    TxEntry · TxLog · TxID · TxResult · Message            |
  |             ExternalTx · UnsignedTx · InternalTx · MessageID       |
  +--------------------------------------------------------------------+
                                     | persisted via
                                     v
  +--------------------------------------------------------------------+
  |  storage    ActorID · Actor · ActorRegistry · VbytePool · Env      |
  +--------------------------------------------------------------------+
                                     | supported by
                                     v
  +--------------------------------------------------------------------+
  |  support    VMError · CheckedFee · Address · encoding fns          |
  +--------------------------------------------------------------------+
```

Reading down: each layer depends only on the layers below it. `Value` is the
convergence point — most data / linear / CS types are reachable only as one of
its variants. The constraint-system types are the only ones gated by execution
context (the `Delegate` seam / `is_external`).

---

## 1. Value -- the stack element (`value.rs`)

`Value` is the central union; every opcode pushes and pops `Value`s.

```
Value (enum, 13 variants)
|-- Int253(Int253)
|-- String(String)
|-- Dict(Dict)
|-- Point(Point)
|-- Token(Token)
|-- WideToken(WideToken)
|-- ClearToken(ClearToken)
|-- Cell(Cell)
|-- Merlin(Merlin)
|-- Variable(Variable)
|-- Expression(Expression)
|-- Constraint(Constraint)
`-- MultiscalarMul(MultiscalarMul)
```

Classifier methods sort the variants: `try_clone` (VM copyability / linear-type
gate), `is_portable` (storage gate), `is_droppable`. See spec §Stack discipline.

---

## 2. Plain-data types (`int253.rs`, `string.rs`, `dict.rs`, `crypto.rs`)

| Type   | Defined   | Depends on                                           | Notes                                                                                                     |
|--------|-----------|------------------------------------------------------|-----------------------------------------------------------------------------------------------------------|
| Int253 | int253.rs | Scalar (dalek)                                       | 253-bit sign-magnitude scalar; the numeric primitive.                                                     |
| String | string.rs | Int253, Point, Commitment, Predicate, Script         | Byte-string with prover-side typed witnesses (Opaque / Scalar / Point / Commitment / Predicate / Script). |
| Dict   | dict.rs   | Int253 (keys), Value (values)                        | Sorted map. **Never copyable; portable-only insert** (todo #5). One sticky `droppable` flag.              |
| Point  | crypto.rs | CompressedRistretto, Box<Commitment>, Box<Predicate> | Opaque / Commitment / Predicate; lazy decompression.                                                      |
| Merlin | crypto.rs | merlin::Transcript                                   | Linear transcript handle (non-copyable).                                                                  |

```
String --+-- Opaque(bytes)
         |-- Scalar(Int253)
         |-- Point(Point) ----------> Point
         |-- Commitment(Commitment) -> Commitment
         |-- Predicate(Predicate) ---> Predicate   (witness; verifier sees Opaque)
         `-- Script(Vec<Instruction>) (prover-side code witness)

Point ---+-- Opaque(CompressedRistretto)
         |-- Commitment(Box<Commitment>) -> Commitment
         `-- Predicate(Box<Predicate>) ---> Predicate
```

---

## 3. Linear token types (`token.rs`)

| Type       | Fields                           | Notes                                                     |
|------------|----------------------------------|-----------------------------------------------------------|
| Token      | qty: Commitment, flv: Commitment | Confidential bearer asset (range-proven at construction). |
| WideToken  | spacesuit::AllocatedValue        | Non-portable intermediate (qty may be negative).          |
| ClearToken | qty: Int253, flv: Int253         | Cleartext token.                                          |

Free fn `flavor_from_actor(...)` derives a flavor scalar. All three are linear
(non-copyable); `Token` / non-negative `ClearToken` are portable.

```
Token      --> Commitment  (x2: qty, flv)
ClearToken --> Int253      (x2: qty, flv)
WideToken  --> spacesuit::AllocatedValue
```

---

## 4. Constraint-system types (`constraints.rs`, `msm.rs`)

These exist only in **external context** (Bulletproofs R1CS). The `Delegate`
seam routes them; internal-context use hard-fails `ExternalOnly`.

| Type              | Depends on                                                     |
|-------------------|----------------------------------------------------------------|
| Variable          | Commitment                                                     |
| Expression        | Int253 (constant) · Vec<(r1cs::Variable, Scalar)>              |
| Constraint        | bool (Cleartext) · SecretConstraint                            |
| SecretConstraint  | Expression (Eq) · recursive Box<SecretConstraint> (And/Or/Not) |
| Commitment        | CompressedRistretto (Closed) · Box<CommitmentWitness> (Open)   |
| CommitmentWitness | Int253, Scalar                                                 |
| MultiscalarMul    | Vec of scalar·point terms (dalek)                              |

```
Constraint --+-- Cleartext(bool)
             `-- Secret(SecretConstraint)
                       |-- Eq(Expression, Expression)
                       |-- And(Box<SecretConstraint>, Box<SecretConstraint>)
                       |-- Or (Box<SecretConstraint>, Box<SecretConstraint>)
                       `-- Not(Box<SecretConstraint>)

Variable --> Commitment --+-- Closed(CompressedRistretto)
                          `-- Open(Box<CommitmentWitness>) --> (Int253, Scalar)
Expression --> Int253 · r1cs::Variable · Scalar
```

`Constraint::verify<CS>(cs)` lowers into the R1CS -- the bridge from VM values
to the proof system.

---

## 5. Cells & predicates (`cell.rs`)

| Type                     | Fields / depends on                                                           |
|--------------------------|-------------------------------------------------------------------------------|
| Cell                     | predicate: Predicate, anchor: Anchor, payload: Vec<Value>                     |
| Predicate                | point: CompressedRistretto, witness: Option<Box<dyn PredicateWitness>>        |
| PredicateWitness (trait) | to_point() · clone_witness() · as_any()                                       |
| PredicateTree (impl)     | internal_key · leaves: Vec<PredicateLeaf> · cached point                      |
| PredicateLeaf            | Program(Vec<u8>) · Blinding([u8;32])                                          |
| TaprootProof             | internal_key · neighbors: Vec<[u8;32]> · position: Vec<u8> · program: Vec<u8> |
| CellID                   | = [u8; 32] (type alias)                                                       |

(`TaprootProof` was `CallProof`; renamed todo #6. Its wire form is a 3-entry
list-style `Dict`.)

```
Cell --+-- predicate: Predicate --+-- point: CompressedRistretto
       |                          `-- witness: Box<dyn PredicateWitness>
       |                                    `-- PredicateTree --> PredicateLeaf
       |                                              (Program | Blinding)
       |-- anchor: Anchor              (vm.rs)
       `-- payload: Vec<Value>         (recurses into Value)

TaprootProof --> merkle path, verified against Predicate (open / cell unlock)
```

`Cell::id() -> CellID`, committed by `TxEntry::Input` / `Output`.

---

## 6. Scripts & opcodes (`script.rs`, `ops.rs`)

Renamed/merged in the Script refactor (todo #1-3): `Program` -> `ScriptBuilder`,
`Code` + `ProgramItem` -> one `Script`.

| Type          | Defined   | Depends on                                                                                             |
|---------------|-----------|--------------------------------------------------------------------------------------------------------|
| ScriptBuilder | script.rs | instructions: Vec<Instruction> · loop_scopes: Vec<LoopScope>; the fluent builder + build_* combinators |
| Script        | script.rs | Transparent(Vec<Instruction>) (prover) · Opaque(Vec<u8>) (verifier). The immutable value/exec form.    |
| Instruction   | ops.rs    | Int253 / bytes operands inline; ~100 variants                                                          |
| LoopScope     | script.rs | builder-internal label bookkeeping for break/continue                                                  |

```
ScriptBuilder --build/into_script--> Script --+-- Transparent(Vec<Instruction>)
     |  (parse / build_if / build_while / ...) `-- Opaque(Vec<u8>)
     `-- to_bytecode() --> Vec<u8>

Script is what CallFrame.code holds and what a code-bearing stack Value carries.
```

---

## 7. Execution core (`vm.rs`)

All `pub(crate)` -- invisible downstream. Users reach this layer only through
the outer tx API (section 8).

| Type             | Fields / depends on                                                                                                                                       |
|------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------|
| VM               | header · last_anchor: Option<Anchor> · current_call: CallFrame · call_stack: Vec<CallFrame> · txlog: Vec<TxEntry> · total_fee: CheckedFee · deferred_sigs |
| CallFrame        | stack: Vec<Value> · code: Script · cursor · labels · kind: CallKind · anchor: Option<Anchor> · gas/mem fields · CS snapshots                              |
| CallKind         | ExternalRoot · InternalRoot{actor,caller} · ActorCall{actor,caller} · CellOpen{predicate,external_context}                                                |
| Anchor           | [u8; 32] newtype                                                                                                                                          |
| BlockContext     | height: u64                                                                                                                                               |
| Delegate (trait) | seam: cs() · batch_verifier() · commit_variable() -- external vs internal                                                                                 |
| InternalDelegate | panicking no-op Delegate (internal context has no CS)                                                                                                     |
| DeferredSig      | Explicit{vk,msg,sig} · TxBound{vk,cell_id}                                                                                                                |
| TxResult         | txid · txlog · total_fee · gas_used · vbytes_used · bytecode · proof · deferred_sigs                                                                      |

(`CallKind` no longer carries `anchor` -- moved to `CallFrame.anchor`, todo #4.
`InternalRoot`/`ActorCall` no longer carry `method` -- removed in ADR 0020.)

```
VM --+-- current_call: CallFrame --+-- stack: Vec<Value>
     |                             |-- code: Script (Transparent | Opaque)
     |                             |-- kind: CallKind --> ActorID, Predicate
     |                             |-- anchor: Option<Anchor>
     |                             `-- labels / gas / mem / CS snapshots
     |-- call_stack: Vec<CallFrame>
     |-- txlog: Vec<TxEntry>
     |-- total_fee: CheckedFee
     `-- deferred_sigs: Vec<DeferredSig>

VM::run / step  --(Delegate)-->  Prover | Verifier | InternalDelegate
VM::into_result ------------->   TxResult

Delegate (trait)
  |-- Prover<'g>       cs: r1cs::Prover,   batch: musig::BatchVerifier  (proving)
  |-- Verifier        cs: r1cs::Verifier, batch: musig::BatchVerifier  (verifying)
  `-- InternalDelegate  panics on cs()/commit  (internal actor execution)
```

---

## 8. Transactions & messages (`tx.rs`, `message.rs`)

| Type                | Depends on                                                                                                                                                                      |
|---------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| TxHeader            | version: u32 · locktime: u32                                                                                                                                                    |
| TxEntry (enum)      | Header · Data · Input(CellID) · Receive · Output(Cell) · IssuePub(Int253,Int253) · IssuePriv · Retire · Fee(u64) · ActorSave{actor,state} · SetCode{actor,code} · Send(Message) |
| TxLog               | Vec<TxEntry> (encode-only Encodable, no Decodable)                                                                                                                              |
| TxID                | Hash (merkle root over TxEntry::commit)                                                                                                                                         |
| Message             | target: ActorID · caller: Option<ActorID> · anchor: Anchor · payload: Vec<Value> · gas · vbytes · refund_predicate: Predicate                                                   |
| MessageID           | [u8; 32]  (= H("flamevm.send.id", Message.encode()))                                                                                                                            |
| ExternalTx          | header · script: Vec<u8> · signature: musig::Signature · proof: R1CSProof                                                                                                       |
| UnsignedTx          | header · script · proof · log: TxLog · metrics: TxMetrics · txbound_items                                                                                                       |
| InternalTx          | log: TxLog · metrics: TxMetrics                                                                                                                                                 |
| TxMetrics           | gas_used · total_fee · vbytes_used (copy counters)                                                                                                                              |
| Limits              | gas: u64 · mem: u64                                                                                                                                                             |
| SigningInstructions | txid: TxID · items: Vec<(vk, CellID)>                                                                                                                                           |
| Env (trait)         | working_copy() -> Box<dyn ActorRegistry> · height() · apply_changes(&TxLog)                                                                                                     |

(`Message` lost its `method` field in ADR 0020 -- the dispatch selector now rides
as the topmost `payload` arg. `MessageID` was "SendID"; the wire domain label
`flamevm.send.id` is unchanged.)

```
TxEntry --> (TxHeader | Cell | Message | ActorID | Value | Int253 | CellID | points)
TxLog   --> Vec<TxEntry> --(merkle)--> TxID

Outer API flow:
  ScriptBuilder --build_tx--> UnsignedTx --signing_instructions--> SigningInstructions
                                  |  sign
                                  v
                              ExternalTx --verify(Limits)--> TxLog
  Message --execute_tx(Limits, &Env)--> InternalTx (log + metrics)
```

---

## 9. Storage / actors (`actor.rs`)

| Type                  | Fields / depends on                                                                                                                                                                                                                  |
|-----------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| ActorID               | Hash([u8;32]) · Constructor(Vec<u8>)  -- both to_hash() to the same id                                                                                                                                                               |
| Actor                 | code: Vec<u8> · state: Option<Value> · vbytes · active_blocks · last_activation_height · frozen_since                                                                                                                                |
| ActorRegistry (trait) | state: load_state->Value / save_state(Value); code: load_code / set_code; actor_vbytes / exists; checkpoints: push / pop_commit / pop_rollback; lifecycle: deploy / commit_tx_destructions / credit_vbytes / tick_block / vbyte_pool |
| VbytePool             | available: u64 · maturing: BTreeMap<u64,u64> (block-level vbyte recycling)                                                                                                                                                           |

`MemRegistry` / `MemEnv` (reference impls of `ActorRegistry` / `Env`) live in
`src/tests/mem_registry.rs`, not in the shipped crate.

```
ActorRegistry (trait) --> Actor --+-- state: Option<Value>  (checkout = re-entrancy lock)
                                  `-- code: Vec<u8>         (dispatched by convention; ADR 0018/0020)
Env (trait) --> Box<dyn ActorRegistry> (working copy) --apply_changes(TxLog)--> persisted
```

---

## 10. Support types

| Type                  | Defined                 | Role                                                                                                                                   |
|-----------------------|-------------------------|----------------------------------------------------------------------------------------------------------------------------------------|
| VMError               | errors.rs               | crate-wide error enum; every fallible op returns it                                                                                    |
| CheckedFee / MAX_FEE  | fees.rs                 | overflow-checked fee accumulator (op_fee)                                                                                              |
| Address               | address.rs              | Predicate / Message-target encoding (node/wallet-facing; not wired into op_send -- ADR 0020)                                           |
| Prover<'g> / Verifier | prover.rs / verifier.rs | Delegate impls wrapping r1cs + musig::BatchVerifier                                                                                    |
| encoding fns          | encoding.rs             | write_value / read_value / write_int253 / write_string / list & dict prefixes -- the canonical wire codec used by every Encodable impl |

---

## Dependency direction (no cycles across layers)

```
execution --> program --> value --> {data, linear, cs} --> support
     |                       |
     `------> tx/effects ----+----> storage --> support
```

- `Value` is the convergence point: most data / linear / CS types are reachable
  only as a `Value` variant.
- The CS types (`Variable` / `Expression` / `Constraint` / `Commitment` /
  `MultiscalarMul`) are the only ones gated by execution context (`Delegate` /
  `is_external`).
- `Cell` and `Message` are the two composite carriers that recurse back into
  `Vec<Value>` payloads.
- The outer transaction API (`UnsignedTx` / `ExternalTx` / `InternalTx` / `Env`)
  wraps the inner `VM` / `Prover` / `Verifier` and is the only surface
  integrators use.
