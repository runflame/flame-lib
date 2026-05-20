use crate::Value;

/*

external tx:
  run { run { open-call { run { run } } } }}

internal tx:
  call { run { call { run {...} } } }

*/

// TODO: change this into a trait for prover-side execution.
struct VMRun {
    script: Vec<u8>
}

struct VMCall {
    stack: Vec<Value>,
    runstack: Vec<VMRun>,
    run: VMRun,
}

pub struct VM {
    call_stack: Vec<VMCall>,
    current_call: VMCall,
}


