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

## Definition of Done — phase courante (stub contre contrat)

- [ ] Le binaire appelle `core_version()` + `core_handshake(...).await` via le
  crate, avec un runtime tokio côté front.
- [ ] Aucune logique métier dans le front (tout passe par le noyau).

> La vraie UI (fenêtre GTK4 catalogue + lecture GStreamer) arrive en **Phase 4**.

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
