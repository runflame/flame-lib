use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=tx.ctl");
    let schema = fs::read_to_string("tx.ctl").expect("transaction schema");
    let code = cells::ctl::compile(&schema).expect("valid transaction schema");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"));
    fs::write(output.join("tx_wire.rs"), code).expect("write generated transaction codecs");
}
