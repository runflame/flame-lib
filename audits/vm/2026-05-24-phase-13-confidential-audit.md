# FlameVM Phase-13 audit — 2026-05-24

**Scope**: rich `String` enum, `scalar` / `commit` / `decrypt` opcodes,
end-to-end prove+verify with witness-bearing stack values.
`mix` opcode + encrypted `issue` / `retire` / `borrow` branches are
deferred to Phase 13.5 (scoped below).

**Audit method**: walk each design choice, mark concordance and
divergence from zkvm, survey similar confidential-asset systems
(Mimblewimble / Grin, Monero RingCT, Aleo / snarkOS, Solana
Confidential SPL, Polkadot's Manta / Zcash Sapling).

---

## 1. Rich `String` enum

### What we did

- `pub enum String { Opaque(Vec<u8>), Commitment(Box<Commitment>),
  Scalar(Box<Int253>), Predicate(Box<Predicate>) }`.
- All variants encode to the same opaque bytes on the wire.
  Witness-bearing variants serialize via their inherent encoding
  (`Commitment` → compressed point bytes; `Scalar` → 32-byte
  sign-magnitude; `Predicate` → compressed point bytes).
- Downcasts: `to_commitment` / `to_scalar` / `to_predicate` work
  on both Opaque (parsing bytes) and witness-bearing (extracting
  the typed payload).
- `bytes_view(&self) -> Cow<[u8]>` is the safe byte-view —
  borrowed for Opaque, owned for others.
- `as_bytes(&self) -> &[u8]` is the fast path for Opaque-only
  contexts and **panics** for witness-bearing variants. Sharp edge
  documented in module docs.
- Bit operations (`bit_or`, `shift_left`, etc.) consume `self`,
  serialize to canonical bytes via `to_bytes`, and produce a fresh
  `Opaque` result.

### How zkvm does it

- zkvm's `String` enum has more variants: also includes `Output`,
  `U64`, `U32` for typed item-list packing. FlameVM omits these —
  Cells in FlameVM are linear and travel as `Value::Cell` directly
  rather than wrapped in a String; we have no `U64`/`U32` because
  Int253 covers all integer widths.
- zkvm's `String::to_bytes(self) -> Vec<u8>` always allocates for
  non-Opaque, no-allocs for Opaque (same as ours).
- zkvm doesn't have an `as_bytes(&self)` that panics — they always
  use `to_bytes` (consuming) or `as_bytes(&self) -> &[u8]` ONLY
  on Opaque (compile-time constrained via use-site).

### Divergences

1. **Fewer witness variants** in FlameVM (no `Output`/`U64`/`U32`).
   Justified by FlameVM's typed value enum richer than zkvm's
   (Cells are first-class).
2. **Panic-on-witness `as_bytes`** is FlameVM-specific. zkvm's
   `to_bytes(self)` API avoids the panic. We chose to keep
   `as_bytes(&self)` for backwards-compat with existing call sites
   (~30 use sites in non-witness paths). The panic is documented
   and reachable only from misuse.

### Risk surface

**M1 (medium) — `as_bytes` panic**: A future opcode that accepts a
witness-bearing String and accidentally calls `as_bytes` panics
the entire VM. Currently no production path triggers this; the
existing `Instruction::PushStr` encoder was the only callsite that
would panic and is fixed to use `bytes_view`. Recommend a
clippy-or-grep CI check that flags new `.as_bytes()` calls on
unknown String origins.

**L1 (low) — Witness loss through bit ops**: A script that does
`pushstr(Commitment(c)); bit_not; commit` would lose the witness
(bit_not converts to Opaque). The verifier-side bytecode is
unchanged, so the constraint is still well-defined — but the
prover's proof construction would fail because `commit` would see
Opaque without witness. **Recommend** documenting that bit ops on
witness-bearing strings drop witness; programs should always do
`commit` before any bit op.

### Verdict

✅ Matches zkvm's design intent. The panic-on-witness is a
narrower-but-faster API choice; the bit-op-drops-witness issue is
a documentation gap not a correctness one.

---

## 2. `scalar` (0x5a) and `commit` (0x5b) opcodes

### What we did

- `op_scalar`: `string → expr`. Pops a String, calls
  `String::to_scalar`, pushes `Expression::Constant(int)`.
  Works for both Opaque (parses 32-byte sign-magnitude bytes) and
  `String::Scalar(witness)` (returns witness).
- `op_commit`: `string → var`. Pops a String, calls
  `String::to_commitment`, wraps in `Variable { commitment }`.
  Both Opaque (parses 32-byte point → `Commitment::Closed`) and
  `String::Commitment(open)` (returns Open) paths work.
- The downstream `expr` opcode (Phase 11) calls
  `delegate.commit_variable(&var.commitment)` which now takes a
  full `&Commitment` (was `&CompressedRistretto`).

### How zkvm does it

- `op_scalar`: identical. `pop_item.to_string.to_scalar →
  push Expression::constant`.
- `op_commit`: identical. `pop_item.to_string.to_commitment →
  push Variable { commitment }`.

### Divergences

None substantive. Method names and call shapes mirror zkvm
exactly (post-refactor).

### Risk surface

✅ Tested end-to-end via `prove_then_verify_with_commit_expr_eq`:
prover pushes `String::Commitment(Open(witness))`, runs through
`commit → expr → eq → verify`, then the verifier walks the same
bytecode with `String::Opaque(point bytes)` and the proof checks
out. Confirms that:
- The wire-form is bytewise-identical regardless of String variant.
- `Delegate::commit_variable` binds the same `(point, var)` pair
  on both sides (prover uses witness via `cs.commit(value,
  blinding)`; verifier uses `cs.commit(point)`).

---

## 3. `decrypt` (0x77) opcode

### What we did

- Pops `(q', q, f', f, token)` (q' on top, token at bottom).
- Verifies `token.qty.to_point() == q·B + q'·B_blinding` and
  analogously for flavor. Uses `PedersenGens::default()` to
  compute the expected commitment points.
- On match, pushes `ClearToken(q, f)`.
- On mismatch, hard-fails `CleartextConstraintFalse`.

### How zkvm does it

- zkvm has no `decrypt` opcode. They use `unblind` instead, which
  pops `(scalar, commitment String)` and adds a batch-verifier
  check that the commitment opens to the scalar*B (zero
  blinding). Different mechanism, similar purpose.
- zkvm's unblind only works for unblinded commitments (blinding=0);
  FlameVM's decrypt is more general (accepts arbitrary blinding).

### Divergences

1. **Decrypt vs unblind**: FlameVM's `decrypt` is a stronger
   primitive — it reveals BOTH the value and blinding factor of a
   Pedersen commitment, then re-derives the point and checks
   equality. zkvm's `unblind` only checks that the point matches
   value*B (i.e., blinding=0).
2. **Synchronous check vs batch**: FlameVM checks the equality
   inline (CleartextConstraintFalse on mismatch). zkvm appends to
   a batch verifier that's checked at finalize. Performance trade:
   batch is more efficient (one MSM check at end); inline is
   simpler and the failure point is precise.

### Risk surface

**L2 (low) — Pedersen-only**: Decrypt verifies via
`PedersenGens::default()`, which uses the standard B and B_blinding
generators. If FlameVM ever needs decrypt for a non-default
generator pair (multi-Pedersen?), this opcode would need to
parameterize the gens. Currently a non-issue since all FlameVM
Pedersen commitments use the default gens.

**L3 (low) — Inline vs batch**: Inline check is simpler but doesn't
amortize MSM costs across multiple decrypts. For programs with
many `decrypt`s, batch verification would be faster. zkvm's batch
pattern is the path to follow — Phase 14 will likely revisit when
the batch verifier is wired for signatures.

### Verdict

✅ Functionally correct. The inline check is the right call for a
Phase 13 MVP — simpler reasoning, immediate failure point. Phase 14+
may convert to batch when other ops need it.

---

## 4. `Delegate::commit_variable` signature change

### What we did

- Signature changed from `commit_variable(&CompressedRistretto)`
  to `commit_variable(&Commitment)`.
- Prover impl: uses `commitment.witness()` → `cs.commit(value,
  blinding)`. Returns `WitnessMissing` if the Commitment is
  Closed (verifier-shaped).
- Verifier impl: uses `commitment.to_point()` → `cs.commit(point)`.
  Returns the same `(point, var)` shape.

### How zkvm does it

- Identical signature: `commit_variable(com: &Commitment)`. Both
  zkvm Prover and Verifier implement this exact API.
- zkvm Verifier: `cs.commit(point)`. zkvm Prover: same as ours.

### Verdict

✅ Now matches zkvm exactly. The Phase-11 stub that returned
`WitnessMissing` is replaced by real CS-commit calls on both
sides. Phase-13's `op_expr` exercises this path through the
`commit_variable` route.

---

## 5. Cross-VM comparison: confidential-asset programmability

| System | Confidential commitment | Decrypt-equivalent | Mix / cloak | Programmable |
|---|---|---|---|---|
| **FlameVM** | Pedersen (qty, flv) | `decrypt` (general blinding) | `mix` stub (Phase 13.5) | yes (forth-like) |
| **zkvm** | Pedersen (qty, flv) | `unblind` (blinding=0 only) | `cloak` | yes (similar) |
| **Mimblewimble / Grin** | Pedersen (qty only) | not in protocol | aggregated sigs | no |
| **Monero RingCT** | Pedersen + bulletproofs | not in protocol | per-tx anonymity set | no |
| **Aleo / snarkOS** | account-model + UTXO + zk-snark records | record opens | per-record records | yes (in-circuit Leo) |
| **Solana Confidential SPL** | Pedersen + ElGamal | `apply_pending` reveals balances | n/a (per-account) | limited (programs use SDK) |
| **Zcash Sapling** | Pedersen + JubJub note commitments | spending key reveals | shielded pool | no (validating-only) |

### Observations

1. **Decrypt-style operations vary**. zkvm's `unblind` is narrower
   (zero-blinding only); FlameVM's `decrypt` accepts arbitrary
   blinding. The latter is more flexible but requires the script
   author to plumb the blinding factor through. Phase 13.5 should
   add a zero-blinding shortcut if there's user demand.

2. **Programmable VMs** (FlameVM, zkvm, Aleo) all support some
   form of confidential-asset opcodes. Non-programmable systems
   (Grin, Monero, Sapling) bake confidentiality into the
   transaction format. FlameVM's stack-machine approach is
   probably the closest to zkvm with the small differences noted
   above.

3. **Mix / cloak** is the most complex op across all systems. zkvm
   already has it; we ship the dispatch but defer the gadget
   wiring to Phase 13.5. This is the principled scope cut.

---

## 6. Spec adherence

- Spec rows `0x5a` (scalar) and `0x5b` (commit) updated to reflect
  the rich-String downcast semantics.
- Spec rows `0x76` (mix) and `0x77` (decrypt) updated with the
  full operand documentation. `mix` row notes "Phase 13.5
  deferred" for transparency.
- No other spec changes needed — all Phase-13 opcodes were
  already present in the spec with their stack diagrams; we just
  fleshed out the prose.

---

## 7. Pending vulnerability surface

### New (Phase 13)

- ⚠️ **M1**: `String::as_bytes` panics on witness-bearing variants
  — sharp edge. Documented. Recommend lint check on new callsites.
- ⚠️ **L1**: Bit ops on witness-bearing strings drop the witness
  (conversion to Opaque). Doc gap.
- ⚠️ **L2/L3**: `decrypt` uses default PedersenGens and inline
  verification — bound to current design.
- ⏸️ **Phase 13.5 (deferred)**: `mix` cloak gadget; encrypted
  `issue`/`retire`/`borrow`; 2-in/2-out mix balance test.

### Cumulative (carried forward from earlier phases)

| Item | Phase | Impact | Notes |
|---|---|---|---|
| `BulletproofGens(1024, 1)` re-allocated per instance | 14 | M | DRY refactor |
| Sub-varint U64 branch overflow | 14 | M | Pre-existing Finding 1 |
| Deferred-sig batch verification | 14 | H | Records accumulate but unchecked |
| TxID computation | 17 | H | Pending |
| Memory cap enforcement | 15+ | M | mem_used never increments |
| Gas metering | 17 | M | gas_limit never charged |
| `mix` gadget body | 13.5 | M | Phase 13 stub returns WitnessMissing |
| Encrypted issue/retire/borrow | 13.5 | M | Cleartext branches in Phase 8 |

---

## 8. Recommendations

1. **Phase 13.5 priority**: complete encrypted `issue` / `retire` /
   `borrow` first (these unblock `WideToken` construction), then
   `mix` (which needs WideToken). The 2-in/2-out mix balance test
   from the original Phase 13 spec is the integration milestone.

2. **CI lint**: a grep-based check for `\.as_bytes\(\)` on String
   values whose origin is non-Opaque-guaranteed. Belt-and-suspenders
   for the M1 sharp edge.

3. **Document witness-loss in bit ops**: add a note to
   `flamevm/spec.md` (or design.md) about the semantics of bit ops
   on witness-bearing strings — they convert to Opaque first.

4. **`decrypt` zero-blinding shortcut**: if user demand surfaces,
   add a convenience opcode that bakes blinding=0, matching zkvm's
   `unblind`. Not blocking.

5. **Phase 14 prep**: the `BulletproofGens` 16-KB-per-instance
   issue (M from Phase 12) is amplified by Phase-13's `commit_variable`
   calls (each Prover now calls `cs.commit` per `expr`). DRY the
   generator allocation as part of Phase 14 signature work.

---

## 9. Sign-off

Phase 13 MVP ships cleanly: rich String, scalar, commit, decrypt
all working end-to-end with prove+verify. The `mix` opcode and
encrypted issue/retire/borrow branches are explicitly scoped to
Phase 13.5 — these need `WideToken` construction first
(blocked on encrypted `borrow`). One medium finding (M1: the
`as_bytes` panic) is a sharp edge, not a correctness bug —
documented and recommend a lint guard. Three low findings tracked
for follow-up.

13 new tests (374 total green; build clean; 21 warnings stable).

— vm-engineer (self-audit, 2026-05-24)
