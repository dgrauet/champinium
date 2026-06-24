# CLAUDE.md — Champinium

Plateforme de partage P2P de contenu généré par IA (vidéo, image, audio).
UX esprit Popcorn Time ; architecture à l'opposé : **natif, pas Electron ;
décentralisé jusque dans la découverte, pas d'API centrale.**

> Spec d'architecture complet : `~/Work/.superpowers/champinium/specs/2026-06-24-bootstrap-architecture.md`
> (hors repo — artefact de design local).
> Équipe d'agents, contrat UniFFI et garde-fous : [`AGENTS.md`](AGENTS.md).

## Principes directeurs NON négociables

1. **Natif intégral, 3 OS.** Pas de webview / Electron / Tauri.
   macOS = Swift/SwiftUI · Windows = C#/WinUI 3 · Linux = GTK4 (gtk-rs).
2. **Décentralisé & stateless au maximum.** Aucun composant central obligatoire :
   pas de DB serveur, pas d'API stateful, pas de stockage qu'on possède. Tout état
   vit dans le réseau ou en cache local. Bootstrap & relay résiduels = SANS ÉTAT,
   multipliables trivialement par n'importe qui.

## Architecture : noyau Rust partagé + 3 fronts natifs

Toute la logique vit dans **`crates/champinium-core`** (Rust, tokio/libp2p),
exposée aux UI via **UniFFI**. Le ×3 ne touche QUE la présentation — **aucune
logique métier dans les fronts.**

- **P2P** : rust-libp2p (TCP/QUIC) — Kademlia (provider records), gossipsub,
  bitswap, relay-v2 + DCUtR, identify/ping.
- **Discovery** : provider records Kademlia (qui détient quel CID).
- **Feeds** : records gossip signés (primaire) + IPNS durable (différé post-MVP).
- **Catalogue** : CRDT *maison* reconstruit localement par écoute gossipsub.
- **Stockage** : content-addressed (CID) + cache LRU local.
- **Identité** : Ed25519 → PeerID/DID. Tout contenu et tout feed est SIGNÉ.
- **Lecture** : native par OS — AVPlayer / Media Foundation / GStreamer (pas de hls.js).
- **Modération** : moteur côté nœud, deux checkpoints, denylists signées (voir plus bas).

## Bindings

- Source unique de vérité = `champinium-core`. Bindings **générés au build**
  (jamais commités, voir `.gitignore`).
- Swift : UniFFI → XCFramework. C# : `uniffi-bindgen-cs`. Linux/GTK : consomme le
  crate Rust directement (pas de FFI).
- ⚠️ **Async via FFI = risque technique #1.** Toute fonction async ou stream
  d'événements exposée doit être prototypée et testée TÔT vers Swift ET C#.

## Conventions par langage

- **Rust (core, cli, bootstrap, relay, GTK)** : edition 2021, `cargo fmt` +
  `cargo clippy -- -D warnings` propres avant commit. Erreurs via `thiserror`/
  `anyhow` (jamais d'`unwrap()` sur chemin réseau). Async = tokio. Types exposés
  via UniFFI annotés `#[uniffi::export]` / `#[derive(uniffi::*)]`.
- **Swift (macOS)** : SwiftUI, pas de logique métier (délègue au core). `swift-format`.
- **C# (Windows)** : WinUI 3, MVVM léger, async/await sur les fns du core.
- **Tout front** : si tu écris de la logique non-présentation dans un front,
  c'est un bug d'architecture — remonte-la dans le core.

## Modération — garde-fou OBLIGATOIRE (ne pas désactiver)

La suppression centrale est impossible par construction → modération côté nœud,
**active par défaut** :
- **Checkpoint #1 (ingestion locale)** : hash-match vs bases connues → refus si match.
- **Checkpoint #2 (réception, AVANT tout reseed)** : hash-match + denylists signées
  souscrites → DROP, pas de reseed, signalement P2P. S'applique quelle que soit la
  source (l'interop IPFS public expose à du contenu de pairs non-Champinium).
- Denylist par défaut active à l'installation (`deny/`). Modèle fédéré (subjectif
  par nœud), format inspiré des denylists IPFS.

## Risques classés

1. **Persistance** — contenu sans seeder disparaît. Mitigation : seed-what-you-consume
   + réplication opportuniste ; cold storage (Filecoin/Arweave) documenté, non
   implémenté au pilot.
2. **Async FFI** — async/streams tokio → Swift ET C#. Mitigé par le spike Phase 0.
3. **Modération décentralisée** — deux checkpoints, denylists signées.
4. **Recherche décentralisée non résolue** — tags DHT + index local ; limites assumées.
5. **Coûts vidéo** — cold storage documenté, non implémenté.
6. **NAT traversal** — relay-v2 + DCUtR ; relays stateless multipliables.
7. **Signature multi-OS** — notarisation Apple / Authenticode / Flatpak (Phase 6).
8. **Maintenance ×3 UI** — mitigée par zéro logique dans les fronts.

## Structure de répertoires

```
crates/champinium-core/   noyau Rust partagé (UniFFI) — TOUTE la logique
crates/champinium-cli/    outil debug (Phase 1+)
infra/bootstrap/          nœud rendez-vous stateless
infra/relay/              relay NAT stateless
apps/macos/               SwiftUI (SwiftPM/Xcode)
apps/windows/             WinUI 3 (.sln, C#)
apps/linux/               GTK4 (gtk-rs)
bindings/                 généré au build (gitignoré)
deny/                     denylist par défaut signée
docs/                     documentation
```

## Build

`just` orchestre tout. `just build-core` compile le noyau ; `just gen-swift` /
`just gen-csharp` régénèrent les bindings ; `just check` lance fmt+clippy+tests.
Voir le `justfile` à la racine.

## État actuel

**Phase 1 (noyau P2P nu) — faite.** Le noyau implémente : CID content-addressed
(CIDv1 raw/sha2-256, compatible IPFS), blockstore sur disque avec vérification
d'intégrité, identité Ed25519 persistée, et un nœud libp2p (TCP/Noise/Yamux) avec
Kademlia (provider records), identify, ping et un protocole request-response
`/champinium/block/1.0.0` pour le transfert de blocs. `champinium-cli` et le nœud
`champinium-bootstrap` stateless pilotent ce noyau. Démo deux nœuds : voir
[`docs/phase1-demo.md`](docs/phase1-demo.md).

> Note transfert : Phase 1 utilise **request-response** comme transport de blocs
> (interim) ; le passage à **bitswap** est prévu pour une phase ultérieure.

**Phase 2 — en cours.**
- **Modération ✔** (faite en premier) : moteur `moderation` — denylist par défaut
  compilée dans le binaire (non désactivable) + denylists signées Ed25519
  souscrites (modèle fédéré, signature vérifiée). Enforcement aux trois points :
  ingestion (`add`), réception (`get`), service (requête entrante). CLI :
  `--denylist <fichier>`. Voir [`deny/README.md`](deny/README.md).
- **Feeds signés + gossipsub + catalogue ✔** : `feed` (record `champinium-feed/v1`
  signé Ed25519, versionné par `seq`), diffusé en **gossipsub** ; `catalog` (CRDT
  maison last-writer-wins par émetteur) reconstruit en écoutant. Node :
  `publish_feed` / `catalog_entries`. CLI : `catalog --peer …`.
- **Ingestion ffmpeg → HLS ✔** : `ingest` orchestre ffmpeg (segments alignés sur
  keyframes), chaque segment = un bloc CID (checkpoint #1 via `add`), un manifeste
  `champinium-hls/v1` mappe l'ordre/durée aux CIDs. `Node::ingest_file` →
  CID du manifeste ; `Node::fetch_hls` reconstruit un `index.m3u8` jouable. CLI :
  `ingest <fichier>` / `fetch-hls <manifest> --peer … --out …`.
- **Reste** : feed records dans la DHT (PUT/GET), IPNS durable (différé).

La surface **UniFFI reste en v0** (les fronts ne sont pas concernés par les
phases 1-2 côté noyau).

Phasing : 0 (spike async FFI ✔ contrat) → **1 (P2P nu CLI ✔)** → **2 (modération ✔,
feeds/gossipsub/catalogue ✔, ingestion ffmpeg ✔)** → 3 (MVP jouable macOS).
Voir le spec.
