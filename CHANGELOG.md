# Changelog

Toutes les évolutions notables de ce projet sont documentées ici.
Format : [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/) ;
versionnage : [SemVer](https://semver.org/lang/fr/). À partir de la 0.2.0, ce
fichier est maintenu automatiquement par release-please (voir
[ADR-0005](docs/adr/0005-release-please.md)).

## [0.9.0](https://github.com/dgrauet/champinium/compare/v0.8.0...v0.9.0) (2026-07-24)


### Features

* stockage froid — récupération/repli Arweave (feature opt-in, archivage différé) ([#62](https://github.com/dgrauet/champinium/issues/62)) ([5e60433](https://github.com/dgrauet/champinium/commit/5e604330c605d3c155a83f8d692ba9d37c87097d))

## [0.8.0](https://github.com/dgrauet/champinium/compare/v0.7.0...v0.8.0) (2026-07-23)


### ⚠ BREAKING CHANGES

* aperçu de channel par lien — prévisualiser avant de s'abonner (contrat v9) ([#59](https://github.com/dgrauet/champinium/issues/59))

### Features

* aperçu de channel par lien — prévisualiser avant de s'abonner (contrat v9) ([#59](https://github.com/dgrauet/champinium/issues/59)) ([2ee8738](https://github.com/dgrauet/champinium/commit/2ee8738c9d7a74ef216e433ff79a47e7b028a18c))


### Bug Fixes

* **core:** republie les records de feed souscrits (durabilité hors ligne) ([#55](https://github.com/dgrauet/champinium/issues/55)) ([bb48b27](https://github.com/dgrauet/champinium/commit/bb48b273dbb043ab468febe94a8a4eaabb2048f7))

## [0.7.0](https://github.com/dgrauet/champinium/compare/v0.6.0...v0.7.0) (2026-07-23)


### ⚠ BREAKING CHANGES

* channels lot (d) — modération par clé, blocage local, signalements par channel ([#53](https://github.com/dgrauet/champinium/issues/53))
* Node::get ne met plus le bloc consommé en cache et n'annonce plus le nœud comme fournisseur par défaut. Introduit StorePolicy::{Seed, Stream} (crate-interne) et Node::get_with pour choisir explicitement l'ancien comportement. fetch_hls applique Seed aux channels souscrits, Stream sinon — signature publique inchangée. replicate/replicate_under_provided et les flags --replication-target/ --replicate-max du démon champinium-seed sont supprimés ; reseed() ne publie plus de feed (la publication appartient au créateur, pas au démon).
* channels lot (b) — abonnements, vues Abonnements/Explorer, liens de channel ([#51](https://github.com/dgrauet/champinium/issues/51))
* channels lot (a) — feed v3 (identité de channel signée) + contrat FFI v5 ([#49](https://github.com/dgrauet/champinium/issues/49))

### Features

* channels lot (a) — feed v3 (identité de channel signée) + contrat FFI v5 ([#49](https://github.com/dgrauet/champinium/issues/49)) ([fcaa0dd](https://github.com/dgrauet/champinium/commit/fcaa0ddb1826894f906a31581958228b56099876))
* channels lot (b) — abonnements, vues Abonnements/Explorer, liens de channel ([#51](https://github.com/dgrauet/champinium/issues/51)) ([0e26948](https://github.com/dgrauet/champinium/commit/0e26948c40c646ea416ec08b05578134806f5ecf))
* channels lot (c) — seed proactif, quota, éviction ; retrait de seed-what-you-consume ([#52](https://github.com/dgrauet/champinium/issues/52)) ([9073538](https://github.com/dgrauet/champinium/commit/9073538f17691cd74955ef589f0e6ffb528839d7))
* channels lot (d) — modération par clé, blocage local, signalements par channel ([#53](https://github.com/dgrauet/champinium/issues/53)) ([1f8d5f3](https://github.com/dgrauet/champinium/commit/1f8d5f3e3f80f8f85a98f26417955a24220ba8e9))

## [0.6.0](https://github.com/dgrauet/champinium/compare/v0.5.0...v0.6.0) (2026-07-04)


### Features

* **infra:** packaging sans coût de signature (Phase 6, palier gratuit) ([#29](https://github.com/dgrauet/champinium/issues/29)) ([e25d8c6](https://github.com/dgrauet/champinium/commit/e25d8c664334320d0b595aac30c1b9afc4fdb550))


### Bug Fixes

* **infra:** nom du binaire Linux dans le script de packaging ([#31](https://github.com/dgrauet/champinium/issues/31)) ([dbf3fa1](https://github.com/dgrauet/champinium/commit/dbf3fa1c415d6dba8b91ce8efe89afa28811ca00))

## [0.5.0](https://github.com/dgrauet/champinium/compare/v0.4.0...v0.5.0) (2026-07-04)


### Features

* **p2p:** réplication opportuniste des contenus sous-répliqués ([#27](https://github.com/dgrauet/champinium/issues/27)) ([725c080](https://github.com/dgrauet/champinium/commit/725c0800fb0d25a03e29ad2622c3b43daa6011db))

## [0.4.0](https://github.com/dgrauet/champinium/compare/v0.3.0...v0.4.0) (2026-07-04)


### ⚠ BREAKING CHANGES

* recherche décentralisée — feed v2 (métadonnées signées), index local, tags DHT, contrat v4 ([#24](https://github.com/dgrauet/champinium/issues/24))

### Features

* recherche décentralisée — feed v2 (métadonnées signées), index local, tags DHT, contrat v4 ([#24](https://github.com/dgrauet/champinium/issues/24)) ([23cfd14](https://github.com/dgrauet/champinium/commit/23cfd146fa37dff465585d28bd36294135ad7ec3)), closes [#20](https://github.com/dgrauet/champinium/issues/20)

## [0.3.0](https://github.com/dgrauet/champinium/compare/v0.2.0...v0.3.0) (2026-07-04)


### Features

* **p2p:** signalement P2P des contenus bloqués + réplication mesurée ([#19](https://github.com/dgrauet/champinium/issues/19)) ([96f4f09](https://github.com/dgrauet/champinium/commit/96f4f099d4333e0802c1462c2ffde2169cfc0e51))

## [0.2.0](https://github.com/dgrauet/champinium/compare/v0.1.0...v0.2.0) (2026-07-03)


### ⚠ BREAKING CHANGES

* **ffi:** contrat v3 — FfiError typé + flux d'événements catalogue ([#10](https://github.com/dgrauet/champinium/issues/10))

### Features

* **ffi:** contrat v3 — FfiError typé + flux d'événements catalogue ([#10](https://github.com/dgrauet/champinium/issues/10)) ([40f39b4](https://github.com/dgrauet/champinium/commit/40f39b43dd516606f0c7cf5d6449bb1294ff6cf7)), closes [#6](https://github.com/dgrauet/champinium/issues/6) [#8](https://github.com/dgrauet/champinium/issues/8)
* **p2p:** peer scoring gossipsub + validation applicative des feeds ([#13](https://github.com/dgrauet/champinium/issues/13)) ([cb2ab6c](https://github.com/dgrauet/champinium/commit/cb2ab6c639002cd075630d612371cfef57c8faca)), closes [#7](https://github.com/dgrauet/champinium/issues/7)


### Bug Fixes

* traite les points mineurs de l'audit (core + infra + fronts) ([#4](https://github.com/dgrauet/champinium/issues/4)) ([c013462](https://github.com/dgrauet/champinium/commit/c013462b3a01877ca46fbac6746605e40ca948d9))

## [0.1.0](https://github.com/dgrauet/champinium/compare/v0.1.0...v0.1.0) (2026-06-25)


### Miscellaneous Chores

* fix release-please for initial release ([#1](https://github.com/dgrauet/champinium/issues/1)) ([7f677b5](https://github.com/dgrauet/champinium/commit/7f677b54078b1622dea62fb7d59e25ea712d7581))

## [Non publié]

### Ajouté
- Noyau P2P : CID content-addressed, blockstore, identité Ed25519, Swarm libp2p
  (Kademlia provider records, identify, ping), transfert de blocs
  request-response, CLI de debug, nœud bootstrap stateless.
- Modération côté nœud : denylists signées Ed25519, denylist par défaut non
  désactivable, enforcement à l'ingestion / réception / service.
- Feeds signés (gossipsub + records DHT), catalogue CRDT reconstruit, seq de feed
  persistant.
- Ingestion ffmpeg → HLS adressé par CID ; reconstruction jouable.
- Contrat UniFFI v1 (objet `ChampiniumNode`) ; front macOS (SwiftUI/AVPlayer),
  front Linux (GTK4/GStreamer), front Windows (WinUI 3) — présentation pure.
- Relay NAT (circuit relay v2 + DCUtR) ; démon de seeding `champinium-seed` +
  services launchd/systemd/Windows.
- Fetch concurrent multi-fournisseurs + réannonce (seed-what-you-consume).
- CI multi-OS (Linux/macOS/Windows) + build du front GTK et de l'app macOS.

### Modifié
- Socle réseau remonté à libp2p 0.56.

### Différé
- bitswap : bloqué en amont (dépendance `core2` yankée) — voir
  [ADR-0006](docs/adr/0006-block-transport.md).
