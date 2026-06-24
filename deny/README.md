# Denylists

Modération **côté nœud, fédérée** — le seul mécanisme possible sur un réseau
décentralisé où la suppression centrale n'existe pas par construction.

- `default-denylist.json` — liste **par défaut**, active à l'installation,
  **non désactivable**. Chargée par le moteur de modération du noyau.
- Format `champinium-denylist/v1` : objet **signé** (Ed25519) listant des CIDs /
  hashes bloqués. Inspiré du format de denylist IPFS.
- **Modèle fédéré** : chaque nœud peut souscrire à des listes additionnelles
  (modération subjective par nœud). La liste par défaut reste toujours active.

## Application (deux checkpoints — voir CLAUDE.md)

1. **Ingestion locale** : tout média matché est refusé à la publication.
2. **Réception (avant reseed)** : tout bloc matché est droppé, jamais reseedé,
   et peut déclencher un signalement P2P. S'applique quelle que soit la source.

> Squelette : structure et format figés ici. La signature, la vérification et
> le hash-matching seront implémentés en Phase 2 (checkpoint #1) et Phase 5
> (multi-listes, signalement).
