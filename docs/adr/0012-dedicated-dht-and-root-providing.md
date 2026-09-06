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
- **Rupture protocolaire dure et définitive** : le protocole Kademlia
  `/champinium/kad/1.0.0` sépare pour toujours les nœuds pré- et
  post-mise-à-jour — ils ne partagent plus aucune DHT (ni provider records,
  ni feeds/denylists publiés en DHT). Le reste de la pile (`identify`, topics
  gossipsub) étant inchangé, un ancien et un nouveau nœud continuent de se
  connecter et d'échanger des feeds par gossip : le catalogue d'un ancien
  nœud se peuple d'entrées émises par un créateur récent, mais **leur contenu
  n'est jamais récupérable** (aucun fournisseur découvrable) — un échec
  silencieux plutôt qu'un rejet net. Voir la note de mise à niveau dans
  [`docs/deploy-bootstrap-relay.md`](../deploy-bootstrap-relay.md).

## Limites

- Un segment récupéré sans indice de racine n'est pas découvrable seul : il
  faut passer par les fournisseurs du manifeste (`get --root`), pas par ses
  propres fournisseurs (il n'y en a pas).
- Un manifeste sans fournisseur coûte deux requêtes DHT (fournisseurs du
  root, puis repli sur les fournisseurs du CID) avant de conclure à
  `NoProviders`.
- **Détenteur partiel du manifeste** : si un seeder B détient le manifeste
  d'une publication mais a perdu (ou pas encore récupéré) un de ses segments,
  un pair C qui interroge les fournisseurs du manifeste tombe sur B, échoue
  la requête de ce segment, et le repli sur les fournisseurs du CID nu est
  structurellement vide dans un réseau entièrement à jour (les fournisseurs
  d'une publication SONT les fournisseurs de son manifeste, par construction
  de l'annonce par racine) — `BlockNotFound`, pas `NoProviders` : le repli de
  récupération froide (ADR 0008, réservé à `NoProviders`) ne se déclenche
  QUE si aucun fournisseur n'a pu être interrogé du tout (root et CID nu tous
  deux sans fournisseur), jamais dans ce cas précis où un fournisseur du root
  existe mais a perdu le segment.
- Un bloc à la fois racine nue (ajoutée par `add`, jamais indexée comme
  segment d'une publication) et segment d'une publication indexée est exclu
  de la réannonce par `reprovide_all` (collision de contenu identique, donc
  très improbable ; le contenu reste servi normalement, juste pas réannoncé
  sous cet aspect de « root indépendante »).
- Les segments mis en cache par un `open_stream(Seed)` interrompu avant sa
  fin ne sont pas encore indexés au `SeedIndex` : un redémarrage précoce les
  fait réannoncer comme des racines par `reprovide_all` (connu, mineur).
