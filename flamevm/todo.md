# TODO

All prior items shipped (see plan.md + commits 806685c…fd7b267):

- ~~String::Script naming vs Program — use Script everywhere~~ → `ScriptBuilder` + `Script` (todo #1).
- ~~Program stores builder data → ProgramBuilder type~~ → split into `ScriptBuilder` (build) + `Script` (value) (todo #2).
- ~~Code / ProgramItem doing the same thing~~ → merged into one `Script` enum (todo #3).
- ~~CallKind variants store Anchor~~ → moved to `CallFrame.anchor` (todo #4).
- ~~Dict non-portable/non-copyable hazard~~ → Dict is non-copyable + portable-only insert (todo #5).
- ~~Rename CallProof to TaprootProof~~ → done (todo #6).
