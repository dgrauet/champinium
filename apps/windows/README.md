# Champinium — front Windows (WinUI 3, C#)

Front natif Windows. **Présentation uniquement** : toute la logique vit dans
`champinium-core` (Rust), consommée via les bindings UniFFI C# générés par
`uniffi-bindgen-cs`.

- UI : WinUI 3
- Lecture média : Media Foundation / MediaPlayerElement (pas de hls.js)
- Bindings : générés par `just gen-csharp` → `bindings/csharp/` (namespace
  `Champinium.Core`) + la `champinium_core.dll` native. **Non commités.**

## Structure (cible)

```
apps/windows/
├── Champinium.sln              # solution WinUI 3
└── Champinium/
    ├── Champinium.csproj        # projet WinUI 3 (référence bindings/csharp)
    ├── App.xaml(.cs)
    └── MainWindow.xaml(.cs)
```

Le `.sln` et le `.csproj` complets seront ajoutés quand on attaquera la
présentation Windows (Phase 4). Stub minimal fourni pour figer l'arborescence.

## Build (cible)

```sh
just build-core      # compile le noyau Rust (release) -> champinium_core.dll
just gen-csharp      # génère les bindings C# dans bindings/csharp/
dotnet build apps/windows/Champinium.sln
```

Packaging Phase 6 : MSIX ou MSI + Authenticode.
