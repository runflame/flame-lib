# TODO

- String::Script naming vs Program - maybe use Script everywhere?
- Program - stores builder-related data. Let's have ProgramBuilder type for that?
- Code / ProgramItem - doing the same thing - keeping either opaque bytecode or transparent instructions
- Almost all CallKind variants store Anchor - maybe keep it directly in the VM/CallFrame instance when we instantiate it?
- Dict storing non-portable / non-copyable items may cause hazard: e.g. we use dict for callproof - if it is constructed with a WideToken and then not read fully by the inner VM mechanics, the dict will be discarded with the non-droppable linear type inside. Making all users check the dict's status is dangerous. Instead, we can make all dicts non-copyable always (that's safer because it avoids variable gas costs) and also not allowing non-portable items. Like with Cell payload - dict validates that the item inserted is portable.
- Rename CallProof to TaprootProof.
