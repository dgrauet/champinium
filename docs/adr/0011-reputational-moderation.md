# 0011 — Modération réputationnelle, listes signées distribuées par le réseau

- Statut : accepté (remplace partiellement 0002 : la « denylist compilée »)
- Date : 2026-09-06

## Contexte

L'ADR 0002 posait une denylist de CIDs compilée dans le binaire. Deux
constats : (1) elle était vide et exigeait une release pour évoluer ; (2)
**un CID Champinium ne matchera jamais une base de hash externe** : chaque
CID est un segment HLS issu du réencodage local, et les bases connues sont
perceptuelles. Le hash exact ne protège que contre un contenu précis déjà
vu sur le réseau. Les listes signées existaient mais ne se souscrivaient que
par fichier, sans persistance ni mise à jour.

## Décision

- **Réputationnel primaire** : bannir un émetteur (`key_entries`) est le
  mécanisme premier ; les CIDs restent un complément.
- **Ancre de confiance = clé projet compilée** (`deny/project.issuer`), la
  liste signée est un record DHT `/champinium/denylist/<peerid>` (v3, `seq`
  signé, LWW), suivi périodiquement, mis en cache sur disque, republié par
  les abonnés. Éditeurs tiers souscrits par clé, persistés. L'éditeur projet
  ne peut pas être retiré par l'API ; un binaire recompilé le peut, comme
  toujours.
- Pas de topic gossip (latence de minutes acceptable), pas d'HTTPS (API
  centrale), **pas de hash perceptuel** (lot dédié si un jour).

## Conséquences

- Mise à jour de la liste projet sans release ; protection conservée hors
  ligne grâce au cache.
- Un nœud neuf n'est protégé qu'après la première récupération (ou jamais si
  aucune liste n'est publiée sous la clé projet) — état documenté dans le
  volet des fronts (« jamais récupérée »).
- La clé privée projet est un secret opérationnel hors dépôt ; sa rotation
  exige une release (nouvelle valeur compilée).
- Contrat FFI v13, denylist v2 rejetée (zéro-compat).
