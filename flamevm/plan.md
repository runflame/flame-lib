# FlameVM development plan

Derived from `todo.md` (2026-06). Six cleanups; four design forks resolved
interactively, two folded with obvious defaults. Each phase keeps the suite
green and lands as its own one-line commit. Phases are ordered isolated-first,
big-rename-last, so each diff stays reviewable.

## Decisions

| # | todo item | Decision |
|---|---|---|
| 1 | `String::Script` vs `Program` naming | **Script everywhere** — `Program`→builder rename, unify the vocabulary on "Script". |
| 2 | `Program` holds builder data | **Split builder from value** — mutable `ScriptBuilder` emits an immutable `Script`. |
| 3 | `Code` vs `ProgramItem` duplication | **Merge into one type** — a single two-form `Script` value, used by both the frame executor and the stack-value layer. |
| 5 | Dict can hold non-portable/linear values | **Non-copyable + portable-only insert** — mirror `Cell` payload; consensus-affecting. |
| 4 | Anchor stored in every `CallKind` | *(folded)* **Move to `CallFrame`** — drop `anchor` from the 3 variants, hold one frame field. |
| 6 | `CallProof` name | *(folded)* **Rename to `TaprootProof`** — pure rename, no wire change. |

### Target vocabulary (after the Script refactor)

- **`Script`** — the immutable value/exec form, two variants:
  `Instructions(Vec<Instruction>)` (prover, witness-bearing) and
  `Bytes(Vec<u8>)` (verifier, decode-on-demand). **Replaces both `Code`
  (vm.rs) and `ProgramItem` (program.rs).** `CallFrame` holds a `Script`;
  a stack `Value` that carries code carries a `Script`.
- **`ScriptBuilder`** — the mutable builder: `instructions` + `loop_scopes` +
  the `build_*` combinators. `.build()` → `Script::Instructions(_)`. **Replaces
  today's `Program`.**
- **`String::Script(Script)`** — the witness-bearing String variant now wraps a
  `Script` (was `Vec<Instruction>`), so there is exactly one notion of "script".
- `Instruction` keeps its name.

---

## Phase 1 — Rename `CallProof` → `TaprootProof`

Pure rename; the wire form (a 3-entry list-style Dict) is unchanged.

- `cell.rs`: `struct CallProof` → `TaprootProof`; all impls/refs; doc comments
  ("CallProof" → "Taproot proof").
- `lib.rs`: export rename.
- `vm.rs`, `verifier.rs`, `string.rs`: `to_callproof`/`verify_callproof` →
  `to_taproot_proof`/`verify_taproot_proof` (or keep verb, swap noun).
- `spec.md`: update references in §open / §Predicates.
- tests: `test_cells.rs`, `test_helpers.rs` — type + helper name.

**Verify:** `cargo test -p flamevm` green; `grep -rn CallProof src/` empty.
**Commit:** `cell: rename CallProof → TaprootProof (todo #6)`

## Phase 2 — Dict: non-copyable, portable-only insert

Consensus-affecting — touches the storage gate and golden vectors.

- `dict.rs`:
  - `insert`/`insert_strict` reject non-portable values → `NonPortableInDict`
    (new `VMError`), mirroring `Cell::new`'s payload check.
  - `is_copyable()` → always `false`; drop the `copyable` flag and its
    `absorb_flags` branch. `try_clone` → `Err(TypeNotCopyable)` unconditionally
    (a Dict never duplicates — kills variable-gas + linear-leak hazards).
  - Keep `is_droppable`/`is_portable` only as needed; `portable` is now an
    invariant (always true post-insert) so the field can go too.
- `value.rs`: `Value::Dict` arms for `try_clone`/`is_copyable` collapse to the
  non-copyable path; `getdup` on a dict value now always errors.
- **Audit the `TaprootProof`/callproof path**: it builds a list-style Dict —
  confirm its entries (neighbor hash Strings, position bytes, program bytes) are
  all portable so construction still succeeds. (Expected: yes.)
- `spec.md`: §Dict — state "Dicts are non-copyable and accept only portable
  values (like Cell payloads)."
- tests: update `test_dict_ops.rs` (`getdup_copyable` etc. flip to error cases),
  add `dict_insert_rejects_non_portable`; regenerate `test_golden.rs` only if a
  pinned Dict encoding shifts (encoding itself is unchanged — likely no regen).

**Verify:** suite green; new reject test passes; callproof path still builds.
**Commit:** `dict: non-copyable + portable-only insert; mirror Cell payload (todo #5)`

## Phase 3 — Move `anchor` out of `CallKind` into `CallFrame`

Internal (pub(crate)) refactor; removes the repeated field from 3 variants.

- `vm.rs`:
  - Drop `anchor: Anchor` from `InternalRoot` / `ActorCall` / `CellOpen`.
  - Add `CallFrame.anchor: Option<Anchor>` (`None` for `ExternalRoot`, which
    seeds from `op_input` into `VM.last_anchor`).
  - `kind.anchor()` accessor → `current_call.anchor` read; update
    `op_anchor` / `op_selfid` / child-anchor derivation / `execute_internal`
    frame construction / `enter_cell_open_frame`.
  - `CallFrame::new` / `from_bytecode` / `from_code` take the anchor (or set it
    alongside `kind`).
- tests: `test_helpers.rs`, `test_dispatch.rs`, `test_differential.rs`,
  `test_actor_introspection.rs` construct `CallKind::InternalRoot{…}` — drop the
  `anchor` field there and pass it via the frame.

**Verify:** suite green; anchor-bearing introspection tests
(`selfid`/`anchor`) unchanged in behavior.
**Commit:** `vm: hold frame anchor on CallFrame, not in every CallKind (todo #4)`

## Phase 4 — Script refactor (naming + builder split + Code/ProgramItem merge)

The big one — items 1, 2, 3 are one coordinated change to the code-representation
cluster. Largest churn (renames `Program`, used by ~every test). Done last so
all earlier phases land against stable names.

**Step 4a — introduce the unified `Script` value type.**
- New `Script` enum (`Transparent(Vec<Instruction>)` | `Opaque(Vec<u8>)`) with
  the methods today split across `Code` and `ProgramItem`
  (`to_bytecode`, `into_instructions`, length, decode-on-demand).
- Replace `Code` in `vm.rs` (`CallFrame.code: Script`) and `ProgramItem` in
  `program.rs` / `value.rs` (stack values carry `Script`).

**Step 4b — split `Program` → `ScriptBuilder` + `Script`.**
- `ScriptBuilder` owns `instructions` + `loop_scopes` + `build_*` combinators;
  `parse` and the per-opcode builder methods move here. `.build()` /
  `.into_script()` → `Script::Transparent(_)`; `.to_bytecode()` stays.

**Step 4c — apply "Script everywhere" renames.**
- `Program` → `ScriptBuilder` (builder) / `Script` (value) at all call sites.
- `ProgramItem` removed (folded into `Script`); `String::Script(Vec<Instruction>)`
  → `String::Script(Script)`.
- `lib.rs` exports; `ops.rs` (`compile_instructions` stays); `spec.md` prose
  ("program"→"script" where it means bytecode); **every test** (`Program::new()`
  → `ScriptBuilder::new()`).

**Verify:** `cargo test -p flamevm` green; `grep -rn 'ProgramItem\|enum Code\b' src/`
empty; differential test (`Instructions` vs `Bytes`) still passes — it directly
exercises the merged type.
**Commit:** `script: unify Program/Code/ProgramItem → Script + ScriptBuilder (todo #1–3)`

> Naming collision note: a `String::Script` *variant* and a `Script` *type*
> coexist. Acceptable — the variant is always qualified (`String::Script`). If it
> reads poorly during 4c, fall back to `String::Code(Script)` for the variant.

---

## Sequencing rationale

1→2→3 are isolated and low/medium risk; they bank wins and shrink the surface
before Phase 4's wide rename. Phase 4 is deliberately last so its ~all-tests diff
isn't entangled with semantic changes. Each phase is independently revertible.

## Verification (end to end)

```
cargo build -p flamevm          # zero warnings
cargo test  -p flamevm          # full suite green
cargo build                     # workspace builds (consensus/node consume the API)
grep -rn 'CallProof\|ProgramItem\|enum Code\b' flamevm/src/   # empty
```

Spec stays in lockstep: §open/§Predicates (TaprootProof), §Dict (non-copyable),
control-flow/§send prose ("script"). Regenerate `test_golden.rs` only if a wire
encoding actually shifts (none expected — all six items are type/name/safety
changes, not encoding changes).

---

## Out of scope (tracked elsewhere, not this plan)

Known backlog from the gap-map, gated on Architect decisions or other crates —
listed so this plan's boundary is explicit:

- **vbytes flow** (`vbytes_used`, deploy-credit, `tick_block` driver) — consensus
  crate + a vbyte-purchase spec decision.
- **Empty-send guard** (RECV-style vbyte-only message "cannot fail") — small,
  VM-side, but needs the vbytes-flow decision first.
- **`address.rs` consumer** and **`Message::execute_tx(_limits)`** — wire or
  remove; pending the node API / an Architect call.
- **Chain-info opcodes f2–f7**, **blinded fee**, **bounce path**, **ADR 0009 gas
  calibration** — each its own future workstream.
