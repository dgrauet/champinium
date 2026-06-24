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
  actuel v0 : `CoreVersion()`, `ContractVersion()`, `CoreHandshake(...)` (Task).
- **Produit** : rien pour les autres agents (feuille de l'arbre).

## Definition of Done — phase courante (stub contre contrat)

- [ ] Le projet référence les bindings C# générés et appelle `CoreVersion()` +
  `await CoreHandshake(...)` avec succès.
- [ ] Vérifie `ContractVersion()` au démarrage (détection d'incompat).
- [ ] Aucune logique métier dans le code C#.

> La vraie UI (catalogue + lecture Media Foundation) arrive en **Phase 4**.

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
