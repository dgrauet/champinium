# 0003 — Feeds signés : gossipsub (live) + records DHT (durable)

- Statut : accepté
- Date : 2026-06-24

## Contexte

Le catalogue est décentralisé, sans index serveur. Un créateur doit publier un
feed mutable de ses contenus, découvrable par les autres nœuds.

## Décision

Feed = record `champinium-feed/v1` **signé Ed25519**, versionné par un `seq`
monotone (résolution de conflit last-writer-wins). Publication :
- **gossipsub** (diffusion live, latence en secondes) ;
- **record Kademlia** sous `/champinium/feed/<peerid>` (découverte hors gossip).

Le catalogue est un CRDT « maison » (LWW par émetteur) reconstruit en écoutant.
Le `seq` est **persisté** pour survivre aux redémarrages. IPNS durable est différé.

## Conséquences

- Découverte live ET hors-ligne, authenticité garantie par signature.
- La recherche full-text décentralisée reste non résolue (tags DHT + index local ;
  limites assumées).
