# Audit: `MultiscalarMul` type design

**Date:** 2026-05-26
**Scope:** `flamevm/src/msm.rs`, `value.rs::Value::MultiscalarMul`, `op_add`/`op_neg`/`op_mul`/`op_verify` MSM dispatch, spec.md §MultiscalarMul.
**Background:** ADR-pending. Enables custom Sigma-protocols on encrypted data, batched verification alongside Schnorr/Musig sigs.

## Summary of design

- `MultiscalarMul` is a stack-visible Value variant holding `Vec<(Scalar, CompressedRistretto)>`.
- Linear (non-copyable, non-droppable), non-portable, non-wire-encodable. Type code `0xc3`.
- Construction is implicit through arithmetic ops:
  - `Point + Point` / `Point + MSM` / `MSM + MSM` → MSM (concat).
  - `Int253 * Point` / `Int253 * MSM` → MSM (term / scaled).
  - `neg Point` / `neg MSM` → MSM (negated coefficients).
- `verify MSM` appends to the delegate's `BatchVerifier` with `basepoint_scalar = 0` and the `(scalar, point)` term vector. Asserts `sum == identity` at finalize.
- External-context only (touches the shared CS-batch infrastructure).
- Failure path: a non-identity sum or non-decompressable point surfaces at finalize as `BatchSignatureVerificationFailed`.

## Comparison to Expression

| Property | `Expression` | `MultiscalarMul` |
|---|---|---|
| Algebra | scalars (mod ℓ) | group elements (Ristretto255) |
| Lazy storage | `Vec<(Variable, Scalar)>` + optional `Int253` witness | `Vec<(Scalar, CompressedRistretto)>` |
| Constructors | `alloc(witness)`, `scalar(s)`, lift from Variable | implicit via `add` / `mul` / `neg` from Point or other MSM |
| Linear ops | `+`, `-`, `*` (CS multiplier gate for LC·LC) | `+`, `-`, scalar `·` (no group-element product) |
| Consumed by | `eq` (folds to Constraint), `verify` (via Constraint) | `verify` (direct batch append) |
| Witness preservation | yes — assignment carried for prove | no — only canonical wire-form points kept |
| Context | external only | external only |
| Linearity / portability | linear, non-portable, non-droppable | same |
| Type code | `0xc1` | `0xc3` |

**Differences worth flagging:**

1. **No witness preservation in MSM.** Unlike Expression, the MSM doesn't carry an "expected result" assignment that the prover could check up-front. The prover discovers a bad MSM only at the verifier's batch.verify(). For Expression+Constraint, the prover-side R1CS may detect inconsistency during proof construction (via `prove` returning `R1CSError`). For MSM, no such early signal exists — the prover always succeeds and the verifier always rejects bad MSMs.

   This is consistent with zkvm's signature path (where sigs are deferred too), but it's a behavioral divergence from the Expression family the user said to mirror. Acceptable, but worth documenting in the design FAQ.

2. **MSM has no explicit constructor opcode** (unlike Expression which has `alloc` / `scalar` / `expr`). All MSMs emerge from arithmetic. Pro: minimalism. Con: a script that wants an empty MSM (no terms, trivially identity) has to manufacture one via `0 * P`, which still costs a point decompression. Future could add an explicit `msm` opcode that pushes the empty MSM if the use case appears.

3. **`Variable` analog?** Expression is built atop Variable. MSM has no such intermediate type — the operands are concrete Points (and the prover-side witness in Point is irrelevant past the MSM boundary). This is the right call for Sigma-protocols (the witness is the scalars the prover knows, not the points), but it does mean MSM is more "verifier-facing" than Expression.

## Comparison to zkvm / musig style

- **Batching mechanism:** Identical to how `Signature::verify_batched` and `Multisignature::verify_multi_batched` work in this codebase. `op_verify(MSM)` is a direct caller of `BatchVerification::append`, the same trait method those sigs use. No new batching machinery introduced.

- **Random factor:** Inherited from `starsig::BatchVerifier` (each appended statement multiplied by a fresh random scalar). Provides the same `< 2^-252` security against cross-statement cancellation that signature batches already enjoy.

- **Failure mode:** Same `BatchSignatureVerificationFailed` error variant that signature-batch failures use. Slightly misleading name now (the batch isn't sig-only), but renaming would churn the existing test suite and audit trail. Recommend renaming to `BatchVerificationFailed` in a separate housekeeping commit (or leave as-is and document the broader scope).

- **No transcript involvement in `op_verify(MSM)`.** Signatures bind to Merlin transcripts (TxID for TxBound, program bytes for Explicit). MSMs don't bind to anything — they're just a multiscalar equation. That's correct: a Sigma-protocol's challenges and binding live in the script (via `merlin` / `merlinwrite` / `merlinread` / `mod252`), and the MSM just expresses the final verification equation. The transcript binding is the *script author's* responsibility — this matches the `signcall` model where the script binds itself to context (anchor, actor id) via explicit checks.

- **Mirror to Dalek's `vartime_multiscalar_mul`:** Strauss-style batching gives the ~4× speedup the user mentioned. By routing through `BatchVerifier`, every batched MSM and every batched sig share the same final MSM call, so the speedup compounds across the whole transaction.

## Common-sense critique

### Concerns that should be addressed

1. **`neg Point` lifting to MSM is asymmetric.** Currently `Point` doesn't support any arithmetic except via lifting. So `-Point` produces a single-term `MSM(-1, P)` rather than another Point — there's no `Point::neg` (it'd require a Ristretto decompression + group inversion + recompression). This is fine in practice but means `neg` on a Point is *type-changing*, unlike `neg` on every other type. Worth noting in spec; already done.

2. **`Int253 * Point` reduction.** The `Int253 → Scalar` conversion reduces mod ℓ silently (canonical scalar magnitudes are already < ℓ but a full-magnitude `Int253` could near ℓ). This matches how zkvm's `commit` op silently uses `Int253 → Scalar` via `Prover::commit_variable`. Consistent — but document in spec for completeness.

3. **No protection against malicious MSMs blowing up the batch.** A script can push an MSM with thousands of terms via a loop, then `verify`. Each term costs an entry in `BatchVerifier::dyn_weights/dyn_points` and a random-scalar multiplication at finalize. This is a gas/time vector. Two mitigations available, neither implemented:
   - Per-term gas charge inside the `op_add` / `op_mul` arms when MSM size grows.
   - Cap on `MSM::len()` enforced at construction (e.g., 2^20 terms).
   Recommend adding gas accounting in a follow-up.

4. **Decompression cost is deferred to finalize.** `optional_multiscalar_mul` decompresses each point; that's a vartime Ristretto decompression per term. Combined with the random-scalar phase inside `BatchVerifier::append`, the per-MSM-term cost is `O(1)` group ops at op-verify-time and `O(1)` at finalize, so total batched cost is `O(n)` where n is total terms across all MSMs + sigs. This is what we want.

5. **Identity check at `verify` time?** An attacker can construct an MSM that decompresses fine but doesn't sum to identity; the verifier learns only at finalize. Same property as the signature batching. No bug, just worth noting.

### Concerns that are not concerns

- **Determinism:** The `random_factor` in `BatchVerifier::append` is per-verifier (per ThreadRng), not part of the transaction state. Prover and verifier each have their own batch with their own random factors. As long as each appended statement holds (sum = identity), the batched check passes with overwhelming probability regardless of which factor was drawn. This is the standard batching-soundness argument and is identical to how sigs already work.

- **MSM in inner cell scripts (`open` / `signcall`):** A leaf script can build and verify an MSM exactly like the outer script can, provided the call frame inherits external context (CellOpen with `external_context: true`). The CS / batch verifier are owned by the Delegate, not the frame, so accumulation crosses frame boundaries naturally. ADR 0013 handles this implicitly.

- **`size` opcode:** Doesn't apply to MSM (it returns `TypeHasNoLength`). Internal len would be useful for gas, not for scripts. Don't expose.

## Risk / scope assessment

- **Risk: Low.** New code is self-contained (`msm.rs` is ~110 LOC, ops dispatch additions are ~50 LOC across 3 functions, op_verify gain ~20 LOC). No existing test regressions (525 → 543, +18 new tests passing).
- **Cryptographic risk: Minimal.** All cryptography is delegated to `starsig::BatchVerifier` + Dalek `optional_multiscalar_mul` — well-trodden primitives. The only new construction is the basepoint-coefficient-0 trick for batching pure MSMs, which is a trivial parameter choice.
- **Spec changes: Documented.** Type listing, dedicated §MultiscalarMul section, per-opcode notes on `neg`/`add`/`mul`/`verify`.

## Recommendations

| # | Item | Priority |
|---|---|---|
| 1 | Rename `BatchSignatureVerificationFailed` → `BatchVerificationFailed` (now covers MSMs too) | low (cosmetic) |
| 2 | Add per-MSM-term gas charge in `op_add` / `op_mul` lifts | medium |
| 3 | Cap `MSM::len()` (e.g., `MAX_MSM_TERMS = 2^16`) at construction; hard-fail beyond | medium |
| 4 | Write an example Sigma-protocol test (e.g., proof of correct ElGamal re-encryption) using Merlin + MSM end-to-end | medium |
| 5 | Document the user's "is portable" phrasing — what was meant? Current impl is **non-portable** (matches Expression/Constraint); spec.md and code agree, but the user's original prompt said "is portable and is not wire-encodable", which is contradictory under spec.md's definition. Either correct the design (allow MSM to be embedded in cell payload — would need a wire format) or correct the user's wording in the design notes. Recommend the latter. | low (clarify intent) |
| 6 | Consider a dedicated `msm` opcode that pushes an empty MSM, sidestepping the `0 * P` decompression cost for protocols that want a true zero starting point | low |

## Verdict

The design is sound, the implementation is small, and the integration with the existing batch verifier is clean. It mirrors `Expression` closely enough to be predictable, and mirrors `BatchVerifier` closely enough to inherit the cryptographic soundness of the existing signature batching. The recommended follow-ups are quality-of-life and resource-accounting, not correctness fixes.

**Status:** Approved with follow-ups (gas charge + size cap recommended before mainnet).
