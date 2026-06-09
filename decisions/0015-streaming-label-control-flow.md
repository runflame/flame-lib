# ADR 0015 — Streaming label-based control flow

- **Status:** accepted
- **Date:** 2026-06-08
- **Proposers:** Architect, VM Engineer
- **Deciders:** Architect
- **Supersedes / related:** 0013 (predicate-call isolation), 0014 (txlog records effects)

## Context

FlameVM's control flow was a nest of *Runs*: `run` pushed a sub-script
value onto a per-frame run-stack, `loop` rewound the current Run, `switch`
ran one of two pushed sub-script values, and `break:k` unwound `k` Runs.
This had three problems:

1. **Copies.** `switch` materialised *both* branch scripts on the stack to
   pick one; `run` re-parsed its sub-script value on every invocation. Loops
   and conditionals carried sub-programs as linear `String` values.
2. **No offset story that fits both engines.** The prover executes over a
   witness-bearing `Vec<Instruction>` (instruction-index cursor); the
   verifier will eventually stream over raw bytecode (byte-offset cursor).
   A jump operand carrying a byte offset is right for the verifier and
   meaningless to the prover, and vice-versa — any offset bakes a cursor
   space into the consensus bytecode.
3. **Run-stack machinery.** Nested Runs added a second execution-context
   concept on top of CallFrames, with its own unwinding rules.

We want: write bytecode without computing offsets; compose `if`/`while`/
`loop`/`switch` freely; keep the prover/verifier execution identical; and
keep memory and random-access cost low. We also decided (this same cycle)
that **code only ever executes by becoming a CallFrame** — `open`,
`signcall`, `call`, and the external/internal roots — so there is no
inline dynamic-code execution and no general `run` opcode.

## Options considered

1. **Immediate relative offset, validated at parse.** Operand is a signed
   instruction-index delta; one parse pass checks every target is in
   bounds.
   - Pros: static well-formedness (admission can reject); O(1) jumps.
   - Cons: bakes the cursor space into the operand (breaks prover/verifier
     symmetry); requires a full parse pass — incompatible with a streaming
     verifier that never materialises `Vec<Instruction>`.
2. **Numbered labels, sequential, frame-local (chosen).** `label N` records
   "position after this label" into a per-frame array indexed by `N`;
   `jump N` / `jumpif N` resolve via that array, scanning forward to
   discover not-yet-seen labels.
   - Pros: offset-agnostic (each engine records positions in *its own*
     cursor space — instruction index on the prover, byte offset on a
     future streaming verifier); single-pass / no pre-parse; loops and
     conditionals compose without offset arithmetic; backward and
     already-seen jumps are O(1) array indexing.
   - Cons: well-formedness is checked *in-stream* at runtime, not statically
     at admission; a future streaming verifier re-decodes loop bodies each
     iteration.
3. **Structured block markers + runtime block stack.** `if`/`else`/`endif`,
   `loop`/`endloop`, `break`/`continue` with a nesting stack.
   - Pros: O(nesting-depth) memory; structure checkable in-stream.
   - Cons: enforces structured nesting in the VM (less flexible than
     `goto`-style labels); more opcodes; the builder gains nothing the
     label scheme doesn't already give.
4. **Borrow code from the stack.** Frames hold references into stack slots.
   - Cons: FlameVM has no borrows; lifetime/aliasing hazards. Rejected.

## Decision

Replace `run`/`loop`/`switch`/`break:k` (and the per-frame run-stack) with
three opcodes — `label`, `jump`, `jumpif` — each carrying a non-negative
label number as a sub-varint operand. Each CallFrame owns a `labels` array
of positions, frame-local, built lazily:

- **`label N`** records the position *after* the label.
  - `N == labels.len()` → append (the sequential, first-seen case).
  - `N < labels.len()` → a re-visit (loop back-edge re-traversing an inner
    label): permitted iff the recorded position equals the current one;
    otherwise hard-fail `LabelOutOfOrder` (a genuine duplicate at a second
    site).
  - `N > labels.len()` → hard-fail `LabelOutOfOrder` (gap / out of order).
- **`jump N` / `jumpif N`** (the latter pops one `Int253` condition, jumps
  iff non-zero):
  - `N < labels.len()` → set the cursor to `labels[N]` immediately
    (backward, or forward to an already-discovered label).
  - `N >= labels.len()` → enter *skipping mode*: scan forward **without
    executing**, recording every `label` passed (each must be the next
    sequential number), until `label N` is reached, then resume execution
    after it. Reaching the end of the program while skipping hard-fails
    `LabelNotFound`.

Labels are numbered sequentially in bytecode-appearance order (0, 1, 2, …),
so the array is a plain `Vec` indexed directly by label number. The
re-visit clause is what lets loops re-traverse labels inside their own
body — without it, the second iteration of any loop containing an inner
label (i.e. any loop with a nested `if`/loop) would hard-fail.

The label number is offset-agnostic: control flow is driven entirely by
label numbers and stack conditions, both identical on prover and verifier,
so the two engines execute the same path and emit the same constraint
system. The position *representation* (instruction index vs byte offset) is
private to each engine.

## Consequences

- **Positive:**
  - Bytecode carries no offsets; the high-level assembler (`build_if` /
    `build_while` / `build_loop` / `build_switch` / `build_break` /
    `build_continue`) emits labels via a running counter and backpatches
    forward jumps to small integers — no delta arithmetic.
  - The run-stack, the `Run`-nesting concept, and `run`/`loop`/`switch`/
    `break:k` are deleted. Net −1 opcode and one whole execution concept.
  - Single-pass execution; offset-agnostic positions unlock a future
    byte-streaming verifier with no change to the consensus bytecode.
  - Memory is O(#labels actually reached) of positions — far smaller than
    a materialised `Vec<Instruction>`, and lazy.
- **Negative:**
  - Well-formedness (balanced/complete labels, valid jump targets) is
    enforced *in-stream at runtime* (`LabelOutOfOrder` / `LabelNotFound`),
    deterministically on both engines, rather than statically at admission.
    This is the conscious price of dropping the pre-parse pass.
  - Under a future streaming verifier, loop bodies are re-decoded each
    iteration (time-for-memory). The prover re-reads its in-memory `Vec`,
    so this cost is verifier-only.
- **Follow-up work required:**
  - Byte-streaming verifier (decode-as-you-go, `labels` holding byte
    offsets) — separate change to `verifier.rs`; this ADR is the enabler,
    not the implementation. The first cut keeps both engines on
    `Vec<Instruction>` (instruction-index positions).
  - Gas-on-scan: when per-instruction gas metering lands (none today —
    `gas_used` is never incremented; gas is a "long-term cap"), skipping
    mode must charge for scanned instructions so a long skip can't be free.
- **Affected artifacts:** `flamevm/spec.md` §"Control-flow instructions" and
  the instruction table; `flamevm/src/{ops,vm,program,errors}.rs`;
  `flamevm/src/tests/test_control_flow.rs` and Run-driver call sites.

## References

- Design discussion (this session): offset-agnostic indirection, the
  re-visit fix for loop back-edges, prover/verifier cursor asymmetry.
- ADR 0013 — `open`/`signcall` already execute in isolated CallFrames, so
  the run-stack was the last non-call execution context.
- Bitcoin Script `OP_IF`/`OP_ELSE`/`OP_ENDIF` — forward-scan-don't-jump
  precedent for skipping mode.
