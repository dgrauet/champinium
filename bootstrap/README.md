# Bootstraps

`default.peers` liste les nœuds de rendez-vous compilés dans le binaire
(`include_str!`, ADR 0013) : une multiaddr `/…/p2p/<peerid>` par ligne,
`#` = commentaire, lignes vides ignorées. Elle est **vide** tant qu'aucun
bootstrap public n'existe — la liste n'est pas signée, le binaire lui-même
est la confiance.

Un utilisateur peut ajouter ses propres bootstraps (voisins connus, nœud
personnel) via `Node::add_bootstrap` / la CLI, persistés localement dans
`.bootstraps` — ceux-ci ne modifient jamais ce fichier.

## Proposer un bootstrap public

Ouvrir une PR ajoutant une ligne à `default.peers`, avec dans la description
un engagement de disponibilité (uptime visé, contact opérateur). Le nœud doit
écouter sur le port **4101/tcp** (voir
[`docs/deploy-bootstrap-relay.md`](../docs/deploy-bootstrap-relay.md)) et
rester stateless.
