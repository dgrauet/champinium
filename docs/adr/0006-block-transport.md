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

## Point amont (2026-07-04)

`core2` reste entièrement yanké, **mais** `multihash-codetable` a désormais une
lignée **0.2.x saine** (sans `core2` : digest/sha2 0.11). Le blocage se réduit
donc à `beetswap` (0.5.0), qui exige encore `multihash-codetable ^0.1`
(lignée empoisonnée). Prochain déclencheur : un bump `^0.1 → ^0.2` côté
beetswap (demande amont légère), puis reprise de la migration ici — et, dans la
foulée, réévaluation d'IPNS (ADR 0007).

## Note éditoriale (channels lot (c), 2026-07-22)

Le contexte ci-dessus décrit `get()` comme réannonçant systématiquement le bloc
consommé (seed-what-you-consume) : c'était vrai au moment de cette décision,
mais ce n'est plus le comportement par défaut depuis le lot (c) channels — le
retrait de seed-what-you-consume et son remplacement par le seed proactif des
abonnements sont documentés dans `docs/architecture.md` §6. Cette note ne
révise pas la décision ci-dessus (transport request-response, bitswap différé),
qui reste inchangée.
