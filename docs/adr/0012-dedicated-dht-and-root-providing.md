# 0012 — DHT dédiée et annonce par racine

- Statut : accepté
- Date : 2026-09-06

## Contexte

`kad::Config::default()` parlait `/ipfs/kad/1.0.0` : un nœud croisant un pair
kubo rejoignait la DHT IPFS publique (records étrangers stockés chez nous,
nos records `/champinium/…` refusés là-bas) alors que l'interop réelle est
bloquée (ADR 0006/0007). Par ailleurs chaque segment HLS était annoncé
fournisseur : des centaines de records par heure de vidéo et par nœud,
réannoncés périodiquement — le modèle qu'IPFS a abandonné.

## Décision

- Protocole Kademlia dédié `/champinium/kad/1.0.0`. À rouvrir quand
  l'interop IPFS publique sera réelle (bitswap).
- Annonce par **racine** : seuls les roots (manifestes, blocs nus, CIDs de
  feed) sont annoncés ; les segments se récupèrent auprès des fournisseurs
  de leur manifeste (indice de racine dans `get_with`), avec repli sur les
  fournisseurs du CID. `reprovide_all` exclut les segments indexés.

## Conséquences

- Coût DHT divisé par le nombre de segments par publication.
- Un segment sans indice de racine n'est plus découvrable seul (CLI
  `get --root`).
- Réseau Champinium isolé : plus aucun record étranger, plus de dépendance
  aux nœuds kubo.

## Limites

- Un segment récupéré sans indice de racine n'est pas découvrable seul : il
  faut passer par les fournisseurs du manifeste (`get --root`), pas par ses
  propres fournisseurs (il n'y en a pas).
- Un manifeste sans fournisseur coûte deux requêtes DHT (fournisseurs du
  root, puis repli sur les fournisseurs du CID) avant de conclure à
  `NoProviders`.
- Les segments mis en cache par un `open_stream(Seed)` interrompu avant sa
  fin ne sont pas encore indexés au `SeedIndex` : un redémarrage précoce les
  fait réannoncer comme des racines par `reprovide_all` (connu, mineur).
