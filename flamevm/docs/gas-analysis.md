# FlameVM opcode cost & gas-schedule analysis

Status: draft (VM Engineer). Gas parameters need Architect sign-off — see
`feedback/2026-05-30-vm-engineer-on-design.md` and plan.md Phase 40.

Cost figures are **static analysis** — order-of-magnitude cycle counts read
off the implementations, not profiler output. Treat ratios, not absolutes,
as load-bearing. A calibration pass with `criterion` benchmarks should
replace the cycle estimates before the schedule is frozen.

One word = 32 bytes throughout.

---

## 1. Cost model

FlameVM is a ZK-circuit stack machine, so cost lives on **three independent
resource lanes** (already implied by design.md's gas / multiplications /
storage limits):

- **`G` — compute gas.** Interpreter dispatch, hashing, allocation, byte
  copies, BTreeMap ops. Anchored at `G = 1` for a trivial opcode (`nop`).
- **`M` — constraint multipliers.** Bulletproofs multipliers added to the
  R1CS. A per-tx / per-block hard cap, *not* convertible to `G` (it bounds
  proof size and verifier MSM work). Only the CS opcodes spend it.
- **`B` — storage bytes.** Persistent vbytes written (actor state, cells).
  Already metered per-vbyte by ADR 0002 / 0005; listed here for completeness.

### 1.1 Per-opcode cost table

Opcodes grouped by cost profile; every opcode appears once. Cycles are rough.

**Class A — fixed O(1), no heap**
| opcodes | ~cycles | heavy primitive | note |
|---|---|---|---|
| `nop` `push:k` `pushint*` `pushpoint` `pushtoken` `drop` `size` `not` `type` | 5–30 | none | stack-local, hot |
| `abs` `neg`(int) `add`(int) `mul`(int) `and`/`or`(int) `eq`(int) | 20–100 | curve25519 `Scalar` add/mul (const-time) | |
| `mod252` | ~150 | 1 wide modular reduce | input capped ≤64 B |
| `divmod` | 1–3k | 256-iter shift/subtract over 4 limbs | non-const-time, heap-free |
| `method` `gas` `gaslimit` `bytes` `memlimit` `newbytes` `timelock` `version` | 10–40 | none | push one scalar from frame/header |
| `merge` `split` `borrow`(cleartext) `amount` | 20–80 | none | `ClearToken` is `Copy` |
| `loop` `break:k` | 5–20 | none | cursor reset / k pops |

**Class B — fixed, one small alloc**
| opcodes | ~cycles | alloc |
|---|---|---|
| `actorid` `anchor` `callerid` | 50–150 | 1×32 B String |
| `transcript` | ~200 | 1 Strobe state (~200 B) |
| `retire` `fee` `issuepub` `issueprivflv` `issuepubflv` | 100–500 | 1 Box/Transcript/txlog entry; flv ops = 1 Merlin |

**Class C — variable on stack index k**
| opcodes | cost | note |
|---|---|---|
| `roll` `roll:k` | O(k) `Vec::remove` shift | k ≤ stack depth |
| `dup` `dup:k` | O(clone of source) | Int/Point free; String 1 Vec; **Dict recursive clone** — value-driven not k-driven |

**Class D — variable on byte length N (per-byte)**
| opcodes | cost | allocs |
|---|---|---|
| `append` `bitnot` `bitor` `bitand` `bitxor` `writebits` `writeint` `readbits` `readint` `readpoint` | O(N) | 1 fresh Vec(N) |
| `readstr` | O(N) | 2 Vecs (head+tail) |
| `writezeros` | O(N+n) | 2 Vecs |
| `shiftleft` `shiftright` | **O(8N)** per-bit loop | **2 Vecs** |
| `twrite` | O(N) Strobe absorb | reuses Merlin |
| `tread` | O(n) Strobe squeeze | 1 Vec(n) |
| `sha256` `sha3` `keccak256` | O(N) | 1 Vec(32); sha3/keccak heavier const |
| `sha512` | O(N) | 1 Vec(64) |
| `log` | O(N) | 1 Vec(N) into txlog |

**Class E — variable on dict size n (BTreeMap pointer-chasing → cache misses)**
| opcodes | cost | note |
|---|---|---|
| `dict` | O(n) build | 1 BTreeMap(n); values moved |
| `put` `replace` `get` `getopt` `first` `last` `next` | O(log n) | value moved out, no clone |
| `getdup` | O(log n) + recursive clone | deep `try_clone` of one subtree |

**Class F — constraint-system ops (CS multipliers / curve dominate)**
| opcodes | compute | M (multipliers) | note |
|---|---|---|---|
| `scalar` `alloc` | small | 0 | allocates a witness var |
| `commit` `expr` | 50–100k cyc | 0 | **1 Pedersen commit = 1 MSM** |
| `range` | moderate | **64** (+129 constraints) | cleartext short-circuits to 0 |
| `borrow`(encrypted) | ~range | 64 | + 2 commits |
| `issuepriv` | ~range | 64 | + 2 commits + 1 flavor Merlin |
| `decrypt` | 30–60k cyc | 2† | †appends 2 deferred MSM statements; no sync MSM |
| `verify` | O(k) | k† | †appends k MSM terms; final MSM deferred to `finalize` |
| `mix` | O(m+n) | **(m+n) + 64·n** | cloak: ~(m−1)+(n−1) 2-mix gadgets + 3 shuffles + n range proofs + 2(m+n) commits; ~6 O(m+n) Vecs — **dominant variable cost** |

**Class G — cells / actors (payload hashing + curve + state clone)**
| opcodes | cost | driver |
|---|---|---|
| `cell` `output` | O(p) portability + 1 anchor-split Merlin | payload count p (id() deferred to TxID time) |
| `input` | O(payload bytes): decode + `Cell::id()` Merlin | payload bytes |
| `signtx` | O(payload bytes): `cell.id()` Merlin | payload bytes |
| `signcall` | O(bytecode): `signcall_message` Merlin + `Program::parse` | bytecode |
| `send` | O(p) portability + 1 anchor Merlin | payload count p |
| `open` | **curve**: d merkle Merlins + decompress + basepoint scalar-mult + add + compress (~80–120k cyc) + parse O(bytecode) | merkle depth d + bytecode |
| `call` | BTreeMap lookup + script clone + parse O(bytecode) + checkpoint (now O(touched), see §2) | bytecode |
| `load` | `Dict` clone O(state) + checkpoint | state size |
| `save` | `Dict` clone O(state) (txlog) + undo clone O(state) | state size |
| `run` `switch` | verifier `Program::parse` O(bytecode); prover O(1) move | bytecode (verifier) |

### 1.2 Notable hotspots

1. **`mix` / `range`** are the dominant CS-multiplier costs and the most
   important to cap (`m+n` and the implicit per-output range proofs).
2. **`commit` / `expr` / `open`** are the dominant *curve* costs (~1 MSM /
   scalar-mult each).
3. **Hashes, string byte-ops, `twrite/tread`, `log`** are the dominant
   per-byte `G` costs; `shiftleft/right` are 8× worse than needed (§2.3).
4. **`load` / `save` / `dup`(Dict)** clone whole `Dict`s — bounded by the
   `4× vbytes` arena cap (ADR 0002), so per-byte `G` pricing keeps them fair.

---

## 2. Code improvements

### 2.1 Registry checkpoint clone — **FIXED (landed)**
`MemRegistry::push_checkpoint` used to `self.actors.clone()` the **entire**
registry on every `call`/`open`/`load`/`save` frame entry — O(all actor
state), unbounded by the opcode's operands, which made honest per-opcode gas
impossible. Replaced with a **per-frame undo-log** (`CheckpointFrame`): each
frame records the prior value of only the actors/marks it touches, on first
write. Rollback replays the log; commit merges it into the parent frame.
Now O(touched), not O(all actors). Tests: `f1_*`, `f3_*`,
`checkpoint_inner_commit_then_outer_rollback_undoes_save`,
`checkpoint_rollback_removes_actor_deployed_in_frame`.

### 2.2 `op_save` "double clone" — **NOT A BUG (withdrawn)**
On re-inspection, `op_save`'s portability check is `Dict::is_portable()` =
an O(1) sticky-flag read, not a traversal. `op_save` does exactly one
necessary `Dict` clone (the txlog copy, distinct from the registry's owned
copy). No change.

### 2.3 `shiftleft`/`shiftright` — per-bit loop (open)
Both run an O(8N) bit-by-bit loop allocating two Vecs. Replace with
byte/word-level shifting (`copy_within` + a carry word) → O(N) with ~8×
smaller constant and one alloc.

### 2.4 In-place string ops (open)
`append`/`bitnot`/`writezeros` allocate a fresh Vec even when `self` is an
owned `Opaque(Vec<u8>)` consumed by value. Mutate the owned buffer in place
to save one alloc+copy per op.

### 2.5 `Cell::id()` payload buffer (open, minor)
Reuse a single scratch buffer across payload items (it already `clear()`s)
and `reserve` via the `encoded_size_hint` we added, to avoid per-item
reallocs in `input`/`signtx`.

### 2.6 Program parse cache (open, minor)
`call`/`open` re-`Program::parse` the callee bytecode each entry; cache the
parsed `Vec<Instruction>` keyed by method-bytes hash for repeated calls.

---

## 3. Proposed gas schedule

`base + Σ(rate × size)`; rates from §1 ratios (alloc≈3, 32 B hash/Strobe≈2,
curve op≈40, BTree step≈2·log n). `words` = ⌈bytes/32⌉.

| opcode(s) | `G` | `M` | `B` | input limit |
|---|---|---|---|---|
| `nop` `push*` `pushpoint` `pushtoken` `drop` `size` `not` `type` `method` `gas*` `bytes` `memlimit` `newbytes` `timelock` `version` `loop` | 1 | – | – | – |
| `abs` `neg` `add` `mul` `and` `or` `eq`(int) `merge` `split` `borrow`(clear) `amount` | 2 | – | – | – |
| `mod252` | 4 | – | – | ≤64 B (enforced) |
| `divmod` | 8 | – | – | – |
| `roll` `roll:k` `break:k` | 2 + k | – | – | k ≤ 1024 |
| `dup` `dup:k` | 2 + clone·words | – | – | ≤ max string/dict |
| `actorid` `anchor` `callerid` | 4 | – | – | – |
| `transcript` | 6 | – | – | – |
| `twrite` `tread` | 2 + 2·words | – | – | N ≤ 64 KiB |
| `append` `bit*` `write*` `read*` | 2 + 1·words | – | – | N ≤ 64 KiB |
| `shiftleft` `shiftright` | 4 + 2·words | – | – | N ≤ 64 KiB |
| `sha256` `sha3` `keccak256` | 12 + 2·words | – | – | N ≤ 64 KiB |
| `sha512` | 12 + 3·words | – | – | N ≤ 64 KiB |
| `log` | 8 + 1·words | – | words | N ≤ 4 KiB |
| `dict` | 4 + 2·n | – | – | n ≤ 1024 |
| `put` `replace` `get` `getopt` `first` `last` `next` | 4 + 2·log₂n | – | – | n ≤ 1024 |
| `getdup` | 6 + 2·log₂n + clone·words | – | – | |
| `scalar` `alloc` | 6 | – | – | – |
| `commit` `expr` | 40 | – | – | – |
| `range` | 20 | 64 | – | width = 64 fixed |
| `borrow`(enc) | 30 | 64 | – | – |
| `issuepriv` | 60 | 64 | – | – |
| `issuepub` `issueprivflv` `issuepubflv` `retire` `fee` | 10 | – | – | – |
| `decrypt` | 50 | 2 | – | – |
| `verify` | 6 + k | k | – | k ≤ 256 |
| `mix` | 40 + 10·(m+n) | (m+n) + 64·n | – | **m+n ≤ 64** |
| `cell` `output` | 8 + 2·p | – | output: cell vbytes | p ≤ 256 |
| `input` | 30 + 2·words | – | – | payload ≤ 8 KiB |
| `signtx` | 10 + 2·words | – | – | |
| `signcall` | 15 + 2·words | – | – | bytecode ≤ script limit |
| `send` | 12 + 2·p | – | vbytes operand | p ≤ 256 |
| `open` | 100 + 4·d + 2·words | – | – | d ≤ 32 |
| `call` | 50 + 2·words | – | – | call depth ≤ 64 |
| `load` | 20 + 2·words | – | – | state ≤ 4× vbytes |
| `save` | 30 + 2·words | – | words(Δ) | |
| `run` `switch` | 4 + 2·words | – | – | bytecode ≤ script limit |

### 3.1 Input-size limits (DoS backstops)
| dimension | cap | rationale |
|---|---|---|
| string / byte length N | 64 KiB | bounds per-byte `G` (hash, string, Strobe) |
| dict entries n | 1024 | bounds build / log-n ops |
| payload count p (cell/send) | 256 | bounds id-hash & portability scan |
| merkle depth d (open) | 32 | 2³² programs ≫ realistic |
| mix breadth m+n | 64 | bounds cloak `M` blow-up |
| verify MSM terms k | 256 | bounds batch append |
| log data | 4 KiB | non-storage data entry |
| bytecode per script | tx script-size limit | existing block resource |
| actor state | 4× persistent vbytes | ADR 0002 arena cap |

---

## 4. Comparison: Ethereum / TON / Sui

| concern | FlameVM | Ethereum (EVM) | TON (TVM) | Sui (Move) |
|---|---|---|---|---|
| metering | gas + **CS multipliers** + bytes | single gas | single gas, cell-aware | gas = compute + **storage** (+ rebate) |
| arithmetic | `add`/`mul` = 2 (field scalar) | `ADD`=3 `MUL`=5 | `ADD`≈10 `MUL`≈18 | bucketed ~1 |
| hashing | `sha256` 12+2/word | `KECCAK256` 30+6/word; SHA256 precompile 60+12/word | `HASHCU`/`SHA256` ≈ hundreds | native, fixed+per-byte |
| in-mem map | `dict`/`get` 4+2·log n | none (storage only) | `DICTGET` ≈2000+, cell-based | `vector`/`Table` per-elem |
| persistent R/W | `load`/`save` per-word | `SLOAD`=2100 `SSTORE`=20000/5000 | persistent c4 cell + rent | object R/W + rebate |
| object/UTXO create | `cell`/`output` (linear) | none (accounts) | `ENDC` ≈500 | `new`/`transfer` object |
| cross-contract | `call` 50+, `send` async | `CALL` 2600+ + 63/64 stipend | `SENDRAWMSG` async actor | programmable tx block |
| signature | `signtx`/`signcall` batch-deferred (cheap append) | `ECRECOVER` 3000 | `CHKSIGNU` ≈ thousands | native verify |
| **ZK constraints** | `range`/`mix`/`commit` on `M` lane | none | none | none |

**Structural takeaways**
- FlameVM is the only ZK-circuit VM of the four → it needs the `M` lane the
  others lack. `range`/`mix`/`commit`/`issuepriv` have no EVM/TON/Sui
  analogue; their cousins live in zk-rollup *provers*, not base-chain gas.
  This is FlameVM's defining cost axis and should be capped most
  conservatively.
- Closest model is **TON**: stack machine, cell/actor storage, async `send`
  (↔ `SENDRAWMSG`). TON's `DICTGET`≈2000 vs our `get`≈4+2·log n suggests we
  may **under-price** dict ops *if* dicts ever become cell-serialized
  storage; in-memory they're correctly cheap.
- Like **Sui** (and unlike EVM's flat `SSTORE`=20000), FlameVM bills state by
  **bytes touched** — fits per-vbyte actor sizing (ADR 0002); Sui's storage
  *rebate* ≈ actor vbyte recycling (ADR 0005).
- EVM's **quadratic memory expansion** has no FlameVM analogue — transient
  memory is bounded by the `4× vbytes` cap, not priced per word, so the cap
  (not gas) is the transient-memory DoS backstop.

---

## 5. Open questions for the Architect
1. Confirm the **3-lane** model (gas / multipliers / bytes) vs folding
   multipliers into gas. The design's separate "multiplications limit"
   implies 3 lanes; this needs to be explicit in an ADR.
2. Sign off the input-size caps in §3.1 (consensus parameters).
3. `mix` breadth cap (m+n ≤ 64): acceptable for the confidential-tx use
   cases, or too tight?
4. Should `run`/`switch` price the verifier's parse cost (asymmetric
   prover/verifier), or is that absorbed by the script-size limit?
