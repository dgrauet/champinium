# 0001 — libp2p plutôt qu'iroh pour le transport

- Statut : accepté
- Date : 2026-06-23

## Contexte

Deux stacks P2P Rust candidates : **rust-libp2p** (TCP/QUIC, Kademlia, gossipsub,
interop réseau IPFS public) et **iroh** (QUIC, BLAKE3, stack cohérente blobs/
gossip/docs, mais sans interop IPFS public — adressage BLAKE3 vs CIDv1-sha256).

## Décision

Utiliser **rust-libp2p** : provider records Kademlia natifs, gossipsub, et
**interopérabilité avec le réseau IPFS public** (CID, DHT publique, portée Helia).

## Conséquences

- On assemble plus de pièces soi-même (transfert de blocs, feeds, CRDT « maison »).
- Compatibilité CID (CIDv1 raw/sha2-256) avec l'écosystème IPFS.
- Du contenu peut provenir de pairs non-Champinium → la modération s'applique à la
  réception quelle que soit la source (voir [ADR-0002](0002-node-side-moderation.md)).
