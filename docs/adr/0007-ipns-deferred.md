# 0007 — IPNS durable : différé tant que l'interop IPFS public est bloquée

- Statut : accepté
- Date : 2026-07-04
- Issue : #21 (fermée par cette décision — voir note ci-dessous)

> **Note (2026-07-23)** — au moment de cette décision, le point 3 du contexte
> et la conséquence associée (« la durabilité est déjà couverte par … la
> republication du démon de seeding ») étaient **anticipés, pas encore
> vrais** : `champinium-seed` ne réannonçait que les provider records de
> blocs (`Node::reprovide_all`), jamais le RECORD DE FEED signé lui-même
> (`/champinium/feed/<peerid>`) — un créateur qui publiait puis passait hors
> ligne voyait son record de feed s'éteindre au TTL du `MemoryStore`
> Kademlia sans que personne ne le réannonce, cassant la découverte à froid
> de son dernier feed. Cet écart a été corrigé : `Node::republish_known_feeds`
> (appelé par `champinium-seed` dans la même boucle que `reprovide_all`)
> réannonce le feed du nœud lui-même et ceux de ses abonnements. La
> décision de cet ADR (différer IPNS) reste valide : c'est la PRÉMISSE
> « la durabilité pratique est déjà couverte » qui est désormais réellement
> exacte, pas seulement supposée.

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
