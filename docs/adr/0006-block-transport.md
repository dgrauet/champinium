# 0006 — Transport de blocs : request-response (interim), bitswap différé

- Statut : accepté
- Date : 2026-06-25

## Contexte

Le transfert de blocs cible à terme **bitswap** (standard IPFS, multi-fournisseurs).
L'implémentation maintenue, `beetswap`, exige libp2p 0.56 (d'où la montée de
version réalisée). Mais sa dépendance transitive `core2` (via `multihash-codetable
0.1.0`) est **entièrement yankée et sans source git utilisable** → graphe non
résoluble sans vendoring d'un crate no_std mort.

## Décision

Conserver pour l'instant un protocole **request-response** `/champinium/block/1.0.0`
pour le transfert de blocs, et **différer bitswap**. Le `get()` interroge les
fournisseurs **en parallèle** et réannonce le bloc consommé
(seed-what-you-consume), ce qui apporte déjà le bénéfice pratique de bitswap
(récupération multi-sources, réplication).

## Conséquences

- libp2p 0.56 est en place : la migration bitswap ne dépendra plus que de la
  résolution amont de `core2`/`multihash-codetable` (ou d'un vendoring assumé).
- Pas de want-list ni de session bitswap pour l'instant ; transfert bloc-par-bloc.
