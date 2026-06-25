# SPEC — Agent Windows

Voir [`/AGENTS.md`](../../AGENTS.md) pour les règles d'équipe et le contrat.

## Responsabilités

Front natif Windows, **présentation uniquement**. UI C#/WinUI 3 ; lecture média
via Media Foundation / MediaPlayerElement ; intégration d'un **Windows Service**
pour le seeding hors UI. Consomme les bindings C# générés par `uniffi-bindgen-cs`.

## Fichiers possédés

- `apps/windows/**` (.sln, .csproj, sources C#/XAML, futur service Windows)

## Interfaces

- **Consomme** : bindings C# `Champinium.Core` générés par `just gen-csharp`
  (`bindings/csharp/`, **non commité**) + la `champinium_core.dll`. Contrat
  actuel **v1** : objet `ChampiniumNode` (`OpenNode`, `PeerId`, `Catalog`,
  `Listen`, `Connect`, `IngestFile`, `PublishFeed`, `FetchHls`), record
  `FfiCatalogEntry { Issuer, Seq, Cids }`, erreur `FfiError`. Les fonctions libres
  (dont `OpenNode`) sont exposées par uniffi-bindgen-cs dans la classe statique
  `ChampiniumCoreMethods`.
- **Produit** : rien pour les autres agents (feuille de l'arbre).

## Definition of Done — Phase 4 (UI catalogue + lecture)

- [x] Vraie solution WinUI 3 (`Champinium.sln` + `Champinium.csproj`,
  `<UseWinUI>true</UseWinUI>`, `Microsoft.WindowsAppSDK` 1.6.x, cible
  `net8.0-windows10.0.19041.0`), avec `App.xaml(.cs)`, `MainWindow.xaml(.cs)`,
  `app.manifest` et `Package.appxmanifest`.
- [x] UI miroir du `ContentView` macOS : TextBox multiaddr + boutons
  **Connecter**/**Rafraîchir**, liste du catalogue (chaque CID avec bouton
  **Lire**), `MediaPlayerElement` (Media Foundation).
- [x] VM (`NodeViewModel`, MVVM léger via `INotifyPropertyChanged`) : au lancement
  `await OpenNode(<LocalAppData>\Champinium)` puis `Listen("/ip4/0.0.0.0/tcp/0")` ;
  **Connecter** → `Connect` ; **Rafraîchir** → `Catalog` ; **Lire** → `FetchHls`
  puis `MediaPlayerElement.Source = MediaSource.CreateFromUri(...)` + lecture.
- [x] `.csproj` référence les bindings générés
  (`<Compile Include="..\..\..\bindings\csharp\**\*.cs" />`) et copie la dll native
  à côté de l'exe.
- [x] Aucune logique métier dans le code C# (présentation pure ; tout passe par le
  contrat).
- [ ] **Compilation vérifiée** : NON faite ici (dev macOS, pas de SDK .NET/WinUI).
  À valider sur un **runner Windows .NET** (CI Windows / Visual Studio 2022).
  Pré-requis : `just gen-csharp` (bindings) + `just build-core` (dll) d'abord.

> Reste hors périmètre de cette tâche : **Windows Service** de seeding hors UI.

## Ce que l'agent Windows NE doit PAS toucher

- `crates/champinium-core/**` ni aucune autre logique de noyau.
- Les autres fronts, `infra/`, le `justfile`.

## Règles spécifiques

- Besoin d'une capacité absente → **`contract-request:`** à l'agent NOYAU.
- Le Windows Service de seeding doit passer par le noyau ; ne pas contourner la
  modération.
- Packaging/signature (MSIX/MSI, Authenticode) = piloté par l'agent INFRA
  (Phase 6) ; coordination requise.
- ⚠️ async via FFI vers C# = zone de risque #1 : valider tôt `await CoreHandshake`.
