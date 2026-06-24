# SPEC — Agent NOYAU (`rust-core`)

Voir [`/AGENTS.md`](../../AGENTS.md) pour les règles d'équipe et le contrat.

## Responsabilités

Toute la **logique métier** de Champinium, exposée aux fronts via UniFFI :
P2P (rust-libp2p : Kademlia, gossipsub, bitswap, relay-v2/DCUtR), stockage
content-addressed (CID), feeds signés, catalogue CRDT maison, orchestration
ffmpeg (→ HLS), moteur de modération (hash-matching + denylists), identité/clés
Ed25519. **Seul propriétaire de la surface UniFFI** ; définit et **versionne**
le contrat (`CONTRACT_VERSION`).

## Fichiers possédés

- `crates/champinium-core/**` (cœur + `src/bin/uniffi-bindgen.rs` + `uniffi.toml`)
- `crates/champinium-cli/**` (outil de debug du noyau)

## Interfaces

- **Produit** : la surface UniFFI (le contrat — tableau dans `/AGENTS.md`).
  Actuellement v0 : `core_version`, `contract_version`, `core_handshake` (async).
- **Consomme** : le format de denylist signée défini par l'agent INFRA
  (`deny/`) — le noyau **applique** les denylists, l'INFRA en définit le format.

## Definition of Done — phase courante (contrat initial)

- [x] Surface UniFFI v0 compile et expose une fonction **async** (`core_handshake`).
- [x] `CONTRACT_VERSION` exposé via `contract_version()`.
- [x] Tests unitaires du noyau passent (`cargo test -p champinium-core`).
- [x] Bindings Swift **et** C# se génèrent à partir de la lib (async inclus).

## Ce que l'agent NOYAU NE doit PAS toucher

- Le code des fronts (`apps/**`) — il ne connaît que le contrat.
- L'orchestration de build (`justfile`), la CI, le packaging — périmètre INFRA.
- Le **format** de denylist (`deny/`) — périmètre INFRA (le noyau le consomme).

## Règles spécifiques

- Toute évolution de la surface UniFFI : incrémenter `CONTRACT_VERSION`, mettre à
  jour le tableau du contrat dans `/AGENTS.md`, commit préfixé
  `core: contract vN -> vN+1`.
- Aucun chemin de code ne doit permettre d'ingérer ou de reseeder un contenu
  matché par la modération. La modération n'est pas optionnelle.
- Async/streams via FFI = risque #1 : prototyper et tester tôt vers Swift ET C#.
