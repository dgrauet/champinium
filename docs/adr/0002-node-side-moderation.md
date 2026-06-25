# 0002 — Modération côté nœud, active par défaut

- Statut : accepté
- Date : 2026-06-24

## Contexte

Sur un réseau décentralisé ouvert, la suppression centrale d'un contenu est
impossible par construction, et du contenu illégal circulera. C'est une contrainte
de design et un volet juridique (DSA/UE), pas une note de bas de page.

## Décision

Modération **côté nœud, active par défaut et non désactivable** :
- **Denylist par défaut** compilée dans le binaire (inaltérable à l'exécution).
- **Denylists signées Ed25519** souscrites (modèle fédéré), signature vérifiée.
- Enforcement à trois points : **ingestion** (`add`), **réception** (`get`),
  **service** (requête entrante). Un contenu matché n'est jamais stocké, récupéré,
  mis en cache, reseedé ni servi.

## Conséquences

- Réduit (sans éliminer) la circulation de contenu connu illégal sur les nœuds
  conformes.
- Responsabilités documentées dans le README (éditeur, opérateur bootstrap/relay).
- Aucun chemin de code ne contourne les checkpoints (garde-fou inscrit dans AGENTS.md).
