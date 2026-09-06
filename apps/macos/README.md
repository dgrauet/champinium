# Champinium — front macOS (SwiftUI)

Front natif macOS. **Présentation uniquement** : toute la logique vit dans
`champinium-core` (Rust), consommée via les bindings UniFFI Swift.

- UI : SwiftUI
- Lecture média : AVPlayer / AVFoundation (pas de hls.js)
- Bindings : générés par `just gen-swift` → `bindings/swift/` (XCFramework +
  wrapper Swift `ChampiniumCore`). **Non commités** (régénérés au build).

## Build

```sh
just macos-build     # = macos-prepare (bindings + XCFramework) puis swift build
# ou, étape par étape :
just macos-prepare   # noyau release -> bindings Swift -> XCFramework -> copie dans le package
cd apps/macos && swift build   # ou ouvrir Package.swift dans Xcode
```

`macos-prepare` produit (gitignorés) `Frameworks/ChampiniumCoreFFI.xcframework`
et `Sources/ChampiniumCore/ChampiniumCore.swift`.

## UI (Phase 3 MVP)

`ContentView` : barre de connexion à un pair, catalogue reconstruit (via le noyau),
et lecture progressive d'un contenu (`openStream` ouvre une session HLS servie
par le noyau sur `127.0.0.1`, ADR 0009) avec **AVPlayer**, avec une ligne
« segments : x/y » de progression et fermeture de la session (`closeStream`) à
l'arrêt de la lecture. Toute la logique reste dans le noyau ; ce front
n'orchestre que des appels UniFFI.

Packaging Phase 6 : `.app`/`.dmg` + notarisation Apple (Developer ID).
