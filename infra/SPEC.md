# SPEC — Agent INFRA & BUILD

Voir [`/AGENTS.md`](../AGENTS.md) pour les règles d'équipe et le contrat.

## Responsabilités

- **Bootstrap & relay nodes STATELESS** (+ doc « lance le tien »).
- **Denylists signées** : format, outil de publication, **denylist par défaut**
  (le noyau les *applique* ; l'INFRA en définit le *format* et les *publie*).
- **Orchestration du build** (`justfile`) : compiler le noyau → régénérer les
  **3 jeux de bindings** → builder les 3 apps.
- **CI multi-OS** et **packaging/signature** (documentation puis implémentation
  Phase 6).

## Fichiers possédés

- `infra/bootstrap/**`, `infra/relay/**`
- `deny/**` (format + denylist par défaut)
- `justfile`, configuration CI (`.github/` à venir), scripts de packaging

## Interfaces

- **Produit** : `just gen-swift` / `just gen-csharp` / `just gen-bindings` ;
  binaires `champinium-bootstrap` / `champinium-relay` ; format
  `champinium-denylist/v1`.
- **Consomme** : le binaire de génération du noyau
  (`cargo run -p champinium-core --bin uniffi-bindgen`) et `uniffi-bindgen-cs`
  (tag **`v0.9.2+v0.28.3`**, aligné sur uniffi 0.28.3).

## Definition of Done — phase courante (outillage de contrat)

- [x] `justfile` génère les bindings Swift **et** C# à partir de la lib release.
- [x] bootstrap & relay compilent (stubs stateless) sous `infra/`.
- [x] denylist par défaut présente (`deny/default-denylist.json`, format v1).
- [ ] CI multi-OS (build noyau + bindings + lint) — à câbler en Phase 0.

## Ce que l'agent INFRA NE doit PAS toucher

- La **surface UniFFI** / la logique du noyau (`crates/champinium-core` au-delà
  de son invocation pour générer les bindings) — périmètre NOYAU.
- Le code des fronts (`apps/**`) — périmètre des agents OS.

## Règles spécifiques

- **Stateless absolu** : bootstrap/relay ne persistent aucun état nécessaire au
  fonctionnement. N'importe qui doit pouvoir lancer le sien (procédure dans
  `docs/`).
- **Aucun service central obligatoire** introduit dans le build ou le runtime.
- La denylist par défaut est **active et non désactivable** ; ne pas fournir de
  chemin de build qui la retire.
- Bindings générés **jamais commités** (gitignorés) ; régénérés à chaque
  changement de `CONTRACT_VERSION` (déclenché par l'agent NOYAU).
- Tenir à jour la doc « lance ton propre bootstrap/relay » dès leur implémentation.
