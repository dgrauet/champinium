# docs/

Documentation Champinium. À étoffer à mesure des phases.

- Architecture de référence et phasing : voir le spec de design
  (`~/Work/.superpowers/champinium/specs/2026-06-24-bootstrap-architecture.md`,
  artefact local hors repo) et [`../CLAUDE.md`](../CLAUDE.md).

À documenter au fil de l'eau :
- Procédure pour faire tourner son propre bootstrap / relay (Phase 1/4).
- Format de denylist signée et souscription multi-listes (Phase 2/5).
- Limites de la recherche décentralisée (tags DHT + index local) (Phase 5).
- Stockage froid optionnel : décision figée par l'[ADR 0008](adr/0008-cold-storage-arweave.md) (Arweave, créateur-paie, découverte par tags CID) — implémentation différée (lots CS-a/CS-b).
- Lecture progressive par serveur HLS local : décision figée par l'[ADR 0009](adr/0009-progressive-hls-local-server.md), implémentée (contrat FFI v11).
- Provenance déclarée par publication : décision figée par l'[ADR 0010](adr/0010-declared-provenance.md), implémentée (feed v4, contrat FFI v12).
- Modération réputationnelle, listes signées distribuées par le réseau : décision figée par l'[ADR 0011](adr/0011-reputational-moderation.md) (remplace partiellement l'[ADR 0002](adr/0002-node-side-moderation.md)), implémentée (denylist v3, contrat FFI v13).
- Packaging & signature par OS (Phase 6).
- Seeding en arrière-plan : launchd / Windows Service / systemd user (Phase 4).
