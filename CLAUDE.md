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
  `publish_feed` / `catalog_entries`. CLI : `catalog --peer …`. Le `seq` est
  **persisté** (à côté des blocs) : un créateur qui redémarre reprend son `seq`,
  sinon ses nouveaux feeds seraient ignorés par le LWW des pairs.
- **Ingestion ffmpeg → HLS ✔** : `ingest` orchestre ffmpeg (segments alignés sur
  keyframes), chaque segment = un bloc CID (checkpoint #1 via `add`), un manifeste
  `champinium-hls/v1` mappe l'ordre/durée aux CIDs. `Node::ingest_file` →
  CID du manifeste ; `Node::fetch_hls` reconstruit un `index.m3u8` jouable. CLI :
  `ingest <fichier>` / `fetch-hls <manifest> --peer … --out …`.
- **Reste** : feed records dans la DHT (PUT/GET), IPNS durable (différé).

**Phase 3 — en cours.** **Contrat UniFFI étendu v0 → v1** : objet `ChampiniumNode`
(`open_node`, `listen`, `connect`, `catalog`, `ingest_file`, `publish_feed`,
`fetch_hls` — async sauf `peer_id`/`catalog`), record `FfiCatalogEntry`, erreur
`FfiError`. Bindings Swift **et** C# générés et vérifiés pour toute la surface.
**UI macOS (SwiftUI) ✔ (compile)** : `apps/macos` consomme l'XCFramework + le
wrapper généré (`just macos-prepare`) ; `ContentView` fait openNode → listen →
connect → catalogue → `fetchHls` → lecture **AVPlayer**. `swift build` OK
(compile + link contre le binding réel). Lecture GUI à valider hors headless.
Voir [`AGENTS.md`](AGENTS.md) pour le tableau du contrat.

**Phase 4 — en cours.**
- **Front Linux GTK4 (feature `gui`)** : `apps/linux` consomme le crate ; UI =
  ouverture nœud → listen → connect → catalogue → lecture **GStreamer**
  (`playbin`), pont tokio ↔ thread GTK. Gatée par `gui` pour garder le workspace
  vert sans GTK. Compilation `--features gui` non vérifiée en dev macOS.
- **Relay NAT ✔** : circuit relay v2 + DCUtR côté client dans le noyau
  (`with_relay_client`), serveur de relais stateless `relay::start_relay` (qui
  déclare son adresse externe via `add_external_address` — sinon les réservations
  sont acceptées sans adresse). `infra/relay` = binaire réel. **Testé** : un nœud
  derrière « NAT » écoute via circuit, un autre l'atteint *via le relais* et
  récupère un bloc (`block_transfer_over_relay_circuit`).
- **Seeding en arrière-plan ✔** : `Node::reprovide_all` réannonce tous les CIDs
  détenus (le store de providers Kademlia est volatile → indispensable au
  redémarrage). Démon `champinium-seed` (réannonce + republication périodiques,
  hors UI) ; fichiers de service par OS dans `infra/services/` (launchd / systemd
  user / Windows). Testé : `reprovide_makes_stored_blocks_discoverable`.
- **Feed records DHT (PUT/GET) ✔** : `publish_feed` PUT le feed signé dans la
  Kademlia sous `/champinium/feed/<peerid>` ; `Node::fetch_feed` GET + vérifie
  (signature + émetteur) + alimente le catalogue → découverte de feed **hors
  gossip**. CLI : `fetch-feed --issuer <peerid>`. Testé :
  `feed_is_discoverable_via_dht_without_gossip`.
- **Robustesse fetch ✔** : `get()` interroge **tous les fournisseurs en
  parallèle** (première réponse valide gagne) et **réannonce** le bloc consommé
  (le consommateur devient fournisseur → réplication). Testé :
  `consumer_reseeds_to_other_peers`.
- **Reste** : front Windows (en cours), bitswap (remplacer request-response).

Phasing : 0 (spike async FFI ✔ contrat) → **1 (P2P nu CLI ✔)** → **2 (modération ✔,
feeds/gossipsub/catalogue ✔, ingestion ffmpeg ✔)** → **3 (contrat UniFFI v1 ✔,
UI macOS compile ✔)** → 4 (Linux GTK4 ✔, **relay NAT ✔**, **seeding ✔**, Windows à venir).
Voir le spec.
