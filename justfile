# Champinium — orchestration de build.
# Stratégie : compiler le noyau Rust UNE fois, puis régénérer les bindings.
# Source unique de vérité = crates/champinium-core. Bindings générés = gitignorés.

# Détection de l'extension de bibliothèque dynamique selon l'OS.
dylib_ext := if os() == "macos" { "dylib" } else if os() == "windows" { "dll" } else { "so" }
dylib_name := if os() == "windows" { "champinium_core." + dylib_ext } else { "libchampinium_core." + dylib_ext }
dylib := "target/release/" + dylib_name

# Liste les recettes.
default:
    @just --list

# Compile le noyau Rust (release) — produit la lib dynamique consommée par les fronts.
build-core:
    cargo build --release -p champinium-core

# Tout le workspace Rust (core, cli, bootstrap, relay, front Linux).
build-rust:
    cargo build --release

# fmt + clippy strict + tests : à passer avant tout commit.
check:
    cargo fmt --all -- --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test --all

fmt:
    cargo fmt --all

# Génère les bindings Swift (macOS) dans bindings/swift/ via UniFFI library-mode.
# NOTE : assemblage de l'XCFramework (lipo + xcodebuild -create-xcframework) à
# ajouter quand on attaquera le front macOS (Phase 0/3).
gen-swift: build-core
    mkdir -p bindings/swift
    cargo run --release -p champinium-core --bin uniffi-bindgen -- \
        generate --library {{dylib}} --language swift --out-dir bindings/swift

# Génère les bindings C# (Windows) dans bindings/csharp/.
# Requiert : cargo install uniffi-bindgen-cs
gen-csharp: build-core
    mkdir -p bindings/csharp
    uniffi-bindgen-cs --library {{dylib}} --out-dir bindings/csharp

# Régénère tous les bindings.
gen-bindings: gen-swift gen-csharp

clean:
    cargo clean
    rm -rf bindings/
