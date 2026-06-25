# Changelog

Toutes les évolutions notables de ce projet sont documentées ici.
Format : [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/) ;
versionnage : [SemVer](https://semver.org/lang/fr/). À partir de la 0.2.0, ce
fichier est maintenu automatiquement par release-please (voir
[ADR-0005](docs/adr/0005-release-please.md)).

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
