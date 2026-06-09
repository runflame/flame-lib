# ADR 0001 — Rename Object → Cell and Contract → Actor

- **Status:** accepted
- **Date:** 2026-05-22
- **Proposers:** Architect (promoted from existing design discussion)
- **Deciders:** Architect
- **Supersedes / related:** —

## Context

FlameVM's predecessor (zkvm) used the term `Object` for the single-use, payload-bearing
container that an external transaction consumes and produces, and the term `Contract`
for the long-living, addressable entity holding persistent state and methods.

Two problems with the inherited names:

1. **`Object`** collides with the constraint-system type of the same name (the linear
   handle to a commitment used during Bulletproofs accumulation). Two distinct types
   with the same surface name produced repeated reader confusion in the spec and
   in early audit notes.
2. **`Contract`** carries strong implications from Ethereum that do not match Flame's
   semantics: Flame's long-living entity is reactive (`recv` plus method dispatch),
   has a vbyte-priced lifecycle, and is more naturally modeled as an actor in the
   Hewitt sense than as an EVM-style contract. Audiences read "smart contract" and
   import mental models (re-entrancy patterns, gas refunds, fallback functions)
   that we explicitly do not implement.

The two renames have been used consistently in `flamevm/design.md` and `flamevm/spec.md`
since their introduction. This ADR captures the choice in the project's decision log
so that future contributors can see why the older zkvm names do not survive.

## Options considered

1. **Keep `Object` and `Contract`** (status quo before the rename).
   - Pros: zero migration cost; continuity with zkvm.
   - Cons: name collision between two `Object` types in the spec; "contract" imports
     EVM mental models that misrepresent Flame's design.
2. **Rename to `Cell` and `Actor`** (chosen).
   - Pros: each name carries the intended semantics. "Cell" evokes single-use
     containment; "actor" evokes message-driven, addressable, stateful entities.
     Eliminates the `Object` name collision (the constraint-system `Object` type
     keeps its name without ambiguity).
   - Cons: contributors familiar with zkvm must learn the new vocabulary; some
     external literature (Bitcoin UTXO, EVM contracts) uses the old terms.
3. **Rename only one of the two.**
   - Pros: smaller change.
   - Cons: leaves the half-renamed pair fragmenting reader vocabulary; does not
     resolve the strongest case (the `Object` collision).

## Decision

The single-use, payload-bearing container materialized from an output is called a
**cell**. The long-living, addressable, stateful entity holding methods is called an
**actor**. The constraint-system linear handle keeps the name `Object`; there is no
remaining collision because the prior `Object` (now `Cell`) is gone.

These names are load-bearing across `design.md`, `flamevm/spec.md`, all role
documents, and the UI terminology contract. They are not negotiable per-document.

## Consequences

- Positive: terminology is unambiguous in spec and audit prose. UI Designer can
  enforce a single vocabulary across user surfaces.
- Positive: pre-empts mental-model collisions with EVM contracts (re-entrancy,
  fallback functions, refunds) that do not apply to Flame.
- Negative: contributors arriving from zkvm or EVM ecosystems pay a one-time
  vocabulary tax. Mitigated by a glossary in `design.md` and `ui/CLAUDE.md`'s
  terminology contract.
- Follow-up: every existing artifact that says `Object` (meaning the cell) or
  `Contract` (meaning the actor) must be updated. New artifacts must use the
  chosen names from the start.
- Affected artifacts: `design.md` (all sections), `flamevm/spec.md`,
  `flamevm/design.md`, `ui/CLAUDE.md` (terminology contract), all role CLAUDE.md
  files that reference the types.

## References

- `flamevm/design.md` — "Cells" and "Actors" sections.
- `ui/CLAUDE.md` — terminology contract enumerates the canonical names.
- Hewitt, Bishop, Steiger (1973), "A Universal Modular ACTOR Formalism for
  Artificial Intelligence" — origin of the actor model whose semantics Flame
  follows (single inbox, no shared state, message-driven).
