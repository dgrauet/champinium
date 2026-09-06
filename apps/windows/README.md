# Champinium — front Windows (WinUI 3, C#)

Front natif Windows. **Présentation uniquement** : toute la logique vit dans
`champinium-core` (Rust), consommée via les bindings UniFFI C# générés par
`uniffi-bindgen-cs`.

- UI : WinUI 3 (Windows App SDK)
- Lecture média : Media Foundation / `MediaPlayerElement` (pas de hls.js)
- Bindings : générés par `just gen-csharp` → `bindings/csharp/` (namespace
  `Champinium.Core`) + la `champinium_core.dll` native. **Non commités.**

## Structure

```
apps/windows/
├── Champinium.sln               # solution WinUI 3
└── Champinium/
    ├── Champinium.csproj         # projet WinUI 3 (référence bindings/csharp + dll)
    ├── app.manifest              # manifeste Win32 (app non empaquetée, DPI)
    ├── Package.appxmanifest      # identité paquet (pour MSIX éventuel, Phase 6)
    ├── App.xaml(.cs)             # point d'entrée
    ├── MainWindow.xaml(.cs)      # UI : barre connexion + catalogue + lecteur
    └── NodeViewModel.cs          # MVVM léger : orchestration des appels au noyau
```

## Build

⚠️ **Étape obligatoire d'abord** : générer les bindings C# + la dll native, sinon
la compilation échoue (la classe `Champinium.Core.ChampiniumCoreMethods` et le
type `ChampiniumNode` n'existent pas tant que `just gen-csharp` n'a pas tourné).

```sh
# 1. Compile le noyau Rust (release) -> target/release/champinium_core.dll
just build-core

# 2. Génère les bindings C# dans bindings/csharp/ (namespace Champinium.Core)
just gen-csharp

# 3. Compile le front Windows (depuis la racine du repo)
dotnet build apps/windows/Champinium.sln -c Debug -p:Platform=x64
# ... ou ouvrir apps/windows/Champinium.sln dans Visual Studio 2022 (charge utile
#     « Développement d'applications de bureau .NET » + SDK Windows App).
```

Le `.csproj` :

- inclut les bindings via `<Compile Include="..\..\..\bindings\csharp\**\*.cs" />` ;
- copie `target/release/champinium_core.dll` **à côté de l'exe**
  (`CopyToOutputDirectory`). La dll native doit impérativement être dans le même
  dossier que l'exécutable au runtime (P/Invoke).

## Lancement

`OpenNode(<LocalAppData>\Champinium)` au démarrage, puis
`Listen("/ip4/0.0.0.0/tcp/0")`. Coller un multiaddr de pair → **Connecter** ;
**Rafraîchir** relit le catalogue ; **Lire** sur un CID → `OpenStream` ouvre une
session de lecture progressive servie par le noyau sur `127.0.0.1` (ADR 0009),
dont l'URL est passée directement au `MediaPlayerElement` ; une ligne de
progression affiche « segments : x/y » et **Arrêter**/fermeture appelle
`CloseStream`. Chaque entrée du catalogue affiche un badge de provenance
déclarée (« IA / Assisté IA / Capturé / Non déclaré » + outils, ADR 0010) —
pas de champ de saisie, la publication reste CLI-only.

Un volet **« Listes de modération »** (liste projet verrouillée, champ de
collage + « Suivre » pour un éditeur tiers, état « jamais récupérée » tant que
rien n'est en cache) couvre les denylists distribuées par le réseau (ADR 0011).

## Vérification de compilation — IMPORTANT

Ce front a été écrit **sur un poste de dev macOS**, où ni le SDK .NET Windows ni
WinUI 3 ne sont disponibles : **la compilation n'a PAS pu être vérifiée ici.** Le
code C#/XAML a fait l'objet d'une relecture attentive, mais la vérification de
build (`dotnet build`) est **déférée à un runner Windows .NET** (CI Windows ou
poste Visual Studio 2022).

## Packaging

Phase 6 : MSIX ou MSI + Authenticode, piloté par l'agent INFRA. L'app est pour
l'instant **non empaquetée** (`WindowsPackageType=None`).
