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
- Packaging & signature par OS (Phase 6).
- Seeding en arrière-plan : launchd / Windows Service / systemd user (Phase 4).
