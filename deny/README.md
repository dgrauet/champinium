# Denylists

Modération **côté nœud, fédérée** — le seul mécanisme possible sur un réseau
décentralisé où la suppression centrale n'existe pas par construction. Décision
figée par l'[ADR 0011](../docs/adr/0011-reputational-moderation.md) (remplace
partiellement l'[ADR 0002](../docs/adr/0002-node-side-moderation.md)) : le
binaire n'embarque plus de liste, seulement la **clé** d'un éditeur de
confiance ; la liste elle-même est récupérée sur le réseau et peut évoluer
sans nouvelle release.

## 1. Ancre de confiance — `project.issuer` (clé compilée, non désactivable)

- [`project.issuer`](project.issuer) contient le **PeerId** (base58) de
  l'éditeur de la liste de modération du projet. **Compilé dans le binaire**
  du noyau via `include_str!` (`moderation::PROJECT_ISSUER`), donc inaltérable
  à l'exécution → **cet éditeur ne peut pas être retiré** par l'API FFI/CLI
  (`unsubscribe_denylist_issuer` renvoie `InvalidInput` dessus). Un binaire
  recompilé sans cette clé le peut, comme toujours — la protection tient à la
  distribution du binaire officiel, pas à un secret.
- La **clé privée correspondante est hors dépôt** — c'est le seul secret
  opérationnel du projet. Sa rotation change la valeur compilée dans
  `project.issuer` et exige une **nouvelle release**.
- Le binaire n'embarque **aucune liste** : `Moderation::new()` démarre vide.
  Un nœud neuf n'est protégé qu'**après avoir récupéré** la liste projet dans
  la DHT (ou jamais, si aucune liste n'a été publiée sous cette clé) — état
  visible dans les fronts (« jamais récupérée »).

## 2. Format `champinium-denylist/v3`

- **v2 et v1 sont supprimés** : un blob v1/v2 échoue au parsing. Zéro-compat
  descendante, même politique que le feed `champinium-feed/v3`.
- Bannissement par **clé** (`key_entries`, PeerIds) — mécanisme premier — et
  par **CID** (`entries`) en complément. Une clé bannie voit tout son contenu
  refusé, quel que soit le CID.
- **`seq` signé** (u64) : la liste est versionnée, LWW par éditeur — une liste
  reçue avec `seq` ≤ au `seq` connu de cet éditeur est ignorée (protège contre
  le rejeu d'une version antérieure).
- Format :

  ```json
  {
    "schema": "champinium-denylist/v3",
    "name": "...",
    "issuer_pubkey": "<clé publique Ed25519, protobuf libp2p, base64>",
    "seq": 1,
    "updated": "<RFC 3339>",
    "entries": ["<cid>", "..."],
    "key_entries": ["<peerid base58>", "..."],
    "signature": "<signature Ed25519 base64 des octets canoniques>"
  }
  ```

- Octets signés (déterministes, **préfixés par longueur**, non malléables) :
  `schema`, `name`, `updated`, `seq`, puis le nombre de CIDs et les CIDs
  **triés**, puis le nombre de clés et les clés **triées**. `entries` et
  `key_entries` sont couverts **indépendamment**.
- Bornes anti-abus : taille JSON ≤ 1 Mio (`MAX_DENYLIST_SIZE`), `entries` +
  `key_entries` cumulés ≤ 65 536 (`MAX_DENYLIST_ENTRIES`) — vérifiées avant
  tout parsing/prise en compte, y compris à l'entrée du record DHT.

> Une denylist par clé bloque un émetteur nommément désigné (identité vérifiée
> par sa clé). Elle ne fabrique **pas** de liste de CIDs bloqués dérivée des
> feeds : voir `docs/architecture.md` §7 (un tiers ne peut pas censurer un CID
> qu'il ne fait que *lister*, faute de preuve de propriété).

## 3. Distribution — record DHT, cache, suivi

- Une liste signée est publiée dans la DHT sous
  `/champinium/denylist/<peerid-éditeur>` (`Node::publish_denylist`), filtrée
  à l'entrée (taille, signature, émetteur = clé de l'enregistrement).
- Un nœud **souscrit à un éditeur** (`subscribe_denylist_issuer`, persistance
  `.denylist_issuers`) : fetch immédiat, puis suivi **périodique**
  (`FOLLOW_INTERVAL`, même boucle que les abonnements de channel) et
  rattrapage au **démarrage** du nœud.
- **Cache hors ligne** : chaque liste récupérée est mise en cache sur disque
  (`.denylists/<peerid>.json`), rechargé **avant tout réseau** à l'ouverture
  du nœud — la protection tient même sans connectivité. Le cache appliqué au
  démarrage n'exécute pas de purge (les checkpoints lisent les vues agrégées
  du moteur, déjà à jour).
- **Republication** : `Node::republish_known_feeds` (démon `champinium-seed`)
  republie aussi les listes en cache — une liste tierce dont l'éditeur est
  hors ligne ne s'éteint pas au TTL du `MemoryStore` Kademlia tant qu'un
  abonné tourne.
- L'éditeur projet est **toujours réinséré** dans les éditeurs suivis et
  **non retirable** (`InvalidInput` sur `unsubscribe_denylist_issuer`). Une
  liste d'un éditeur **non souscrit** récupérée par ailleurs n'est **jamais
  appliquée**.
- Moteur indexé **par éditeur** : retirer un éditeur retire ses entrées
  seules ; une entrée (CID ou clé) portée par deux éditeurs reste bloquée tant
  qu'un seul des deux la maintient.

## 4. Publier une liste — `denylist sign` / `denylist publish`

Outil éditeur : `champinium-cli denylist sign` génère (ou réutilise) une clé
d'éditeur et produit une liste signée hors nœud, puis `denylist publish` la
diffuse dans la DHT depuis un nœud connecté.

```sh
# Génère la clé si absente, affiche le PeerId à diffuser aux souscripteurs.
champinium-cli denylist sign \
    --key ./editeur.key --name "Liste du projet" --seq 1 \
    --cid <cid-banni> --key-entry <peerid-banni> \
    --out ./liste.json

# Publie depuis un nœud connecté au réseau.
champinium-cli denylist publish ./liste.json --peer <multiaddr-pair>
```

- **`--seq` doit être strictement croissant** à chaque republication de la
  même liste : un `seq` non croissant est traité comme périmé (LWW) et ignoré
  par les nœuds qui connaissent déjà une version plus récente. Convention :
  incrémenter à chaque édition, jamais réutiliser un `seq` déjà publié.
- **Conserver la clé** (`--key`) : c'est l'identité de l'éditeur. La perdre
  revient à ne plus pouvoir publier de mise à jour sous ce PeerId ; la faire
  fuiter permet à un tiers de publier en votre nom.
- **Rotation** : changer de clé change de PeerId — les souscripteurs doivent
  suivre le nouvel éditeur explicitement (`denylist follow`). Pour la clé
  **projet** spécifiquement, la rotation change la valeur compilée dans
  `project.issuer` et exige une nouvelle release du binaire.
- Autres commandes CLI : `denylist sources` (état des éditeurs suivis),
  `denylist follow <lien-ou-peerid>` / `denylist unfollow <peerid>` (suivre /
  cesser de suivre un éditeur tiers), `denylist show <fichier>` (vérifie et
  affiche une liste signée).

## 5. Application (checkpoints — voir CLAUDE.md / SPEC noyau)

1. **Ingestion** (`Node::add`) : un contenu matché (CID ou clé) est refusé —
   ni stocké, ni annoncé.
2. **Réception** (`Node::get`) : un contenu matché n'est ni récupéré, ni mis
   en cache, ni reseedé.
3. **Service** (requête entrante) : un nœud refuse de servir un contenu
   matché.

## 6. Honnêteté sur ce que ça protège

- **Un CID Champinium ne matche aucune base de hash externe.** Chaque CID est
  un segment HLS issu du réencodage local à l'ingestion — les bases connues
  de contenu illégal sont perceptuelles (elles matchent une image/vidéo
  malgré un ré-encodage), un hash exact ne protège que contre un contenu
  *précis déjà vu et listé sur ce réseau*. C'est pourquoi le bannissement par
  **clé** (`key_entries`) est le mécanisme premier : il vise l'émetteur, pas
  un CID précis qui change à chaque réencodage.
- Le hash perceptuel (matcher un contenu recompressé/redimensionné contre une
  base connue) est **différé** — lot dédié si un jour entrepris.
- Un binaire recompilé **sans** la clé projet reste toujours possible : la
  protection tient à la distribution du binaire officiel, pas à un mécanisme
  cryptographique empêchant la recompilation.
