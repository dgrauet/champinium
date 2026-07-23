//! Stockage froid optionnel (Arweave) — ADR 0008.
//!
//! Gaté par la feature cargo `cold-storage` (aucune dépendance tirée par
//! défaut). Vide pour l'instant : le trait `ColdStore` (archive/retrieve),
//! le backend Arweave (signature de transaction hand-roll, cf.
//! `.superpowers/sdd/coldstore-decision.md`) et le repli dans `get_with`
//! arrivent aux tâches suivantes du lot CS-a.
