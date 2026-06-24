# SPEC — Agent macOS

Voir [`/AGENTS.md`](../../AGENTS.md) pour les règles d'équipe et le contrat.

## Responsabilités

Front natif macOS, **présentation uniquement**. UI SwiftUI ; lecture média via
AVPlayer / AVFoundation ; intégration d'un agent **launchd** pour le seeding hors
UI. Consomme les bindings Swift générés à partir du contrat UniFFI.

## Fichiers possédés

- `apps/macos/**` (Package.swift, sources SwiftUI, futur projet Xcode, plist launchd)

## Interfaces

- **Consomme** : module Swift `ChampiniumCore` + `ChampiniumCoreFFI.xcframework`
  générés par `just macos-prepare` (**non commités**). Contrat **v1** : objet
  `ChampiniumNode` (`openNode`, `listen`, `connect`, `catalog`, `ingestFile`,
  `publishFeed`, `fetchHls`) + record `FfiCatalogEntry`.
- **Produit** : rien pour les autres agents (feuille de l'arbre).

## Definition of Done — Phase 3 (MVP macOS)

- [x] Package SwiftPM lie l'XCFramework + le wrapper généré ; `swift build` OK.
- [x] UI SwiftUI : `openNode` → `listen` → `connect` → liste du `catalog` → `fetchHls`
  → lecture **AVPlayer**.
- [x] Aucune logique métier dans le code Swift (orchestration d'appels UniFFI).
- [ ] Exécution/lecture validée sur une vraie session graphique (hors CI headless).

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
