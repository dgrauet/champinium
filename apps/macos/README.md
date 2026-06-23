# Champinium — front macOS (SwiftUI)

Front natif macOS. **Présentation uniquement** : toute la logique vit dans
`champinium-core` (Rust), consommée via les bindings UniFFI Swift.

- UI : SwiftUI
- Lecture média : AVPlayer / AVFoundation (pas de hls.js)
- Bindings : générés par `just gen-swift` → `bindings/swift/` (XCFramework +
  wrapper Swift `ChampiniumCore`). **Non commités** (régénérés au build).

## Build (cible)

```sh
just build-core      # compile le noyau Rust (release)
just gen-swift       # génère XCFramework + wrapper Swift dans bindings/swift/
swift build          # ou ouvrir dans Xcode
```

Packaging Phase 6 : `.app`/`.dmg` + notarisation Apple (Developer ID).
