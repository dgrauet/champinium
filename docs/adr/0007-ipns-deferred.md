# 0007 — IPNS durable : différé tant que l'interop IPFS public est bloquée

- Statut : accepté
- Date : 2026-07-04
- Issue : #21

## Contexte

Le spec prévoit « records gossip signés (primaire) + **IPNS durable
(différé)** » pour les feeds. Aujourd'hui, un feed vit à trois endroits :

1. **gossipsub** (live) — latence en secondes ;
2. **record DHT maison** `/champinium/feed/<peerid>` (PUT/GET signé, validé à
   l'entrée des stores) — découverte hors gossip ;
3. **republication périodique** par `champinium-seed` (et par les nœuds qui
   rediffusent) — les stores DHT étant volatils (MemoryStore), c'est elle qui
   assure la durabilité pratique.

Ce qu'un **vrai** IPNS apporterait par rapport à ça se réduit à un seul
bénéfice : un pointeur **résolvable depuis l'écosystème IPFS public**
(Kubo/Helia) — cold start sans aucun pair Champinium en ligne.

## Décision

**Différer l'implémentation d'IPNS**, à réévaluer quand l'interop IPFS public
redeviendra effective (c'est-à-dire après la migration bitswap, ADR 0006).

Motifs :

1. **La valeur d'IPNS est l'interop, or l'interop est déjà bloquée ailleurs.**
   Sans bitswap (bloqué en amont, ADR 0006), un client IPFS public qui
   résoudrait notre IPNS ne pourrait de toute façon pas récupérer les blocs
   (notre transport de blocs est un protocole request-response maison). Un
   pointeur résolvable vers un contenu irrécupérable n'a pas de valeur.
2. **La durabilité, elle, est déjà couverte** par le record de feed maison +
   la republication du démon de seeding — mêmes garanties pratiques qu'un IPNS
   republié, sans le coût du format (protobuf IPNS, validité/TTL, validateur).
3. **rust-libp2p ne fournit pas IPNS clé en main** : l'implémenter maintenant
   serait un investissement significatif pour le seul bénéfice (1), nul tant
   que (ADR 0006) tient.

## Conséquences

- L'issue #21 est fermée par cette décision ; la réévaluation est adossée à la
  reprise de bitswap (le déclencheur est le déblocage amont de
  `core2`/`multihash-codetable`, suivi dans l'ADR 0006).
- Le jour venu, la question à trancher sera : IPNS **remplace** le record de
  feed maison (format standard, un seul mécanisme) ou le **double** (compat
  interne + interop). La republication restera portée par `champinium-seed`
  dans les deux cas.
- D'ici là, le « cold start » d'un nouveau nœud passe par les bootstraps
  Champinium (docs/deploy-bootstrap-relay.md) — assumé.
