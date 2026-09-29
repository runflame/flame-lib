//! The bindings generator, pinned to the exact UniFFI this crate is built
//! with: `cargo run -p flamewallet-ffi --features cli --bin uniffi-bindgen`.

fn main() {
    uniffi::uniffi_bindgen_main()
}
