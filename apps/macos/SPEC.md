# SPEC — Agent macOS

Voir [`/AGENTS.md`](../../AGENTS.md) pour les règles d'équipe et le contrat.

## Responsabilités

Front natif macOS, **présentation uniquement**. UI SwiftUI ; lecture média via
AVPlayer / AVFoundation ; intégration d'un agent **launchd** pour le seeding hors
UI. Consomme les bindings Swift générés à partir du contrat UniFFI.

## Fichiers possédés

- `apps/macos/**` (Package.swift, sources SwiftUI, futur projet Xcode, plist launchd)

## Interfaces

- **Consomme** : module Swift `ChampiniumCore` généré par `just gen-swift`
  (`bindings/swift/`, **non commité**). Contrat actuel v0 : `coreVersion()`,
  `contractVersion()`, `coreHandshake(_:) async`.
- **Produit** : rien pour les autres agents (feuille de l'arbre).

## Definition of Done — phase courante (stub contre contrat)

- [ ] L'app lie le binding Swift généré et appelle `coreVersion()` +
  `coreHandshake(_:)` (async) avec succès.
- [ ] Vérifie `contractVersion()` au démarrage (détection d'incompat).
- [ ] Aucune logique métier dans le code Swift.

> La vraie UI (catalogue + lecture AVPlayer) arrive en **Phase 3 (MVP)**.

## Ce que l'agent macOS NE doit PAS toucher

- `crates/champinium-core/**` ni aucune autre logique de noyau.
- Les autres fronts (`apps/windows`, `apps/linux`), `infra/`, le `justfile`.

## Règles spécifiques

- Besoin d'une capacité absente du contrat → **`contract-request:`** à l'agent
  NOYAU. Jamais de logique réseau/modération réimplémentée en Swift.
- Le seeding hors UI (launchd) doit utiliser le noyau ; ne pas contourner la
  modération.
- Packaging/signature (.app/.dmg, notarisation) = piloté par l'agent INFRA
  (Phase 6) ; coordination requise.
