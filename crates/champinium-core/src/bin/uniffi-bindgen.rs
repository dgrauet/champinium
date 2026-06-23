//! Point d'entrée de génération des bindings UniFFI (library-mode).
//! Invoqué par le justfile, p.ex. :
//!   cargo run --bin uniffi-bindgen -- generate --library <dylib> --language swift --out-dir <dir>
fn main() {
    uniffi::uniffi_bindgen_main()
}
