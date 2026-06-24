# SPEC — Agent Linux

Voir [`/AGENTS.md`](../../AGENTS.md) pour les règles d'équipe et le contrat.

## Responsabilités

Front natif Linux, **présentation uniquement**. UI GTK4 (gtk-rs) ; lecture média
via GStreamer (hlsdemux) ; intégration d'un **systemd user service** pour le
seeding hors UI. **Consomme le crate `champinium-core` directement** (Rust→Rust,
pas de FFI).

## Fichiers possédés

- `apps/linux/**` (Cargo.toml du front, sources GTK, unit systemd)

## Interfaces

- **Consomme** : le crate `champinium-core` en dépendance Cargo directe (pas de
  bindings générés). Mêmes fonctions que le contrat, en API Rust native :
  `champinium_core::core_version()`, `contract_version()`, `core_handshake(...)`.
- **Produit** : rien pour les autres agents (feuille de l'arbre).

## Definition of Done — Phase 4 (UI GTK4)

- [x] UI GTK4 (feature `gui`) : ouverture nœud → `listen` → `connect` → catalogue
  → lecture GStreamer (`playbin`), via un pont tokio ↔ thread GTK.
- [x] Aucune logique métier dans le front (tout passe par le noyau).
- [x] Build « stub » sans feature reste vert (workspace CI sans GTK).
- [ ] Compilation `--features gui` validée sur Linux (non vérifiée en dev macOS).
- [ ] systemd user service de seeding (à venir).

## Ce que l'agent Linux NE doit PAS toucher

- Le **code interne** de `champinium-core` (il le consomme, ne le modifie pas).
- Les autres fronts, `infra/`, le `justfile`.

## Règles spécifiques

- Consommer le crate ≠ pouvoir le modifier : besoin d'une nouvelle capacité →
  **`contract-request:`** à l'agent NOYAU (même règle que les fronts FFI), pour
  garder une seule frontière de contrat homogène entre les 3 OS.
- Le systemd user service de seeding doit passer par le noyau ; ne pas
  contourner la modération.
- Décommenter la dépendance `gtk4` (libs système requises) seulement au moment
  d'attaquer l'UI (Phase 4) pour garder le squelette buildable.
