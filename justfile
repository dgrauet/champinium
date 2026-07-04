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
# NB : pas de `--all-features` — la feature `gui` du front Linux exige GTK4 +
# GStreamer et casserait sur une machine sans ces libs. Le front GTK est vérifié
# à part (recette `check-linux-gui`, comme le job `linux-gui` de la CI).
check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace

# Clippy + build du front Linux GTK4 (requiert les libs système GTK4/GStreamer).
check-linux-gui:
    cargo clippy -p champinium-linux --features gui -- -D warnings

fmt:
    cargo fmt --all

# Génère les bindings Swift (macOS) dans bindings/swift/ via UniFFI library-mode.
# L'assemblage de l'XCFramework est fait par `macos-prepare` (recette ci-dessous).
gen-swift: build-core
    mkdir -p bindings/swift
    cargo run --release -p champinium-core --bin uniffi-bindgen -- \
        generate --library {{dylib}} --language swift --out-dir bindings/swift

# Prépare le front macOS : bindings Swift + XCFramework + copie dans le package.
macos-prepare: gen-swift
    rm -rf apps/macos/Frameworks apps/macos/Sources/ChampiniumCore build/macos-headers
    mkdir -p apps/macos/Sources/ChampiniumCore apps/macos/Frameworks build/macos-headers
    cp bindings/swift/ChampiniumCore.swift apps/macos/Sources/ChampiniumCore/
    cp bindings/swift/ChampiniumCoreFFI.h build/macos-headers/
    cp bindings/swift/ChampiniumCoreFFI.modulemap build/macos-headers/module.modulemap
    xcodebuild -create-xcframework -library {{dylib}} -headers build/macos-headers \
        -output apps/macos/Frameworks/ChampiniumCoreFFI.xcframework

# Build du front macOS (nécessite macos-prepare au préalable).
macos-build: macos-prepare
    cd apps/macos && swift build

# Bundle macOS distribuable (signature ad-hoc, gratuite — voir docs/packaging.md).
macos-app: macos-prepare
    ./scripts/package-macos-app.sh

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
