# Denylists

Modération **côté nœud, fédérée** — le seul mécanisme possible sur un réseau
décentralisé où la suppression centrale n'existe pas par construction. Deux
niveaux, tous deux appliqués par le moteur du noyau ([`crate::moderation`]).

## 1. Denylist par défaut — `default.cids` (non désactivable)

- Un CID par ligne (`#` = commentaire). **Compilée dans le binaire** du noyau via
  `include_str!`, donc inaltérable à l'exécution → **non désactivable**.
- Toujours active, quel que soit le front. Vide à ce stade (aucune base de hash
  intégrée par défaut) ; les opérateurs y ajoutent des CIDs puis recompilent.

## 2. Denylists signées souscrites — format `champinium-denylist/v2`

- Modèle **fédéré** : un nœud choisit de suivre une ou plusieurs listes tierces
  (modération subjective). Leur **signature Ed25519 est vérifiée** avant prise en
  compte (`Moderation::subscribe`). Une liste non signée ou altérée est rejetée.
- **v2** ajoute le bannissement **par clé** (`key_entries`) : une liste ne bloque
  plus seulement des CIDs, elle peut bannir des **émetteurs entiers** (PeerId) —
  tout contenu émis par une clé bannie est refusé, quel que soit son CID. Le
  format **v1 (CIDs seuls) est supprimé** : zéro compat descendante (même
  politique que le feed `champinium-feed/v3`), un blob v1 échoue dès le parsing
  car `key_entries` est désormais obligatoire.
- Format (voir `default-denylist.example.json` comme gabarit — le suffixe
  `.example` évite de le confondre avec une liste active, sa `signature` étant
  nulle) :

  ```json
  {
    "schema": "champinium-denylist/v2",
    "name": "...",
    "issuer_pubkey": "<clé publique Ed25519, protobuf libp2p, base64>",
    "updated": "<RFC 3339>",
    "entries": ["<cid>", "..."],
    "key_entries": ["<peerid base58>", "..."],
    "signature": "<signature Ed25519 base64 des octets canoniques>"
  }
  ```

- Octets signés (déterministes, **préfixés par longueur** via `push_field`, donc
  non malléables par décalage de frontière) : `schema`, `name`, `updated`, puis
  le nombre de CIDs et les CIDs **triés**, puis le nombre de clés et les clés
  **triées**. `entries` et `key_entries` sont couverts **indépendamment** :
  déplacer une entrée de l'une vers l'autre invalide la signature. La signature
  ne dépend pas de la mise en forme JSON.
- Borne anti-abus : `entries` + `key_entries` cumulés ≤ 65 536
  (`MAX_DENYLIST_ENTRIES`), vérifiée à la souscription.
- Souscription côté CLI : `champinium-cli --denylist liste.json <commande>`.

> Une denylist par clé bloque un émetteur nommément désigné (identité vérifiée
> par sa clé). Elle ne fabrique **pas** de liste de CIDs bloqués dérivée des
> feeds : voir `docs/architecture.md` §7 (un tiers ne peut pas censurer un CID
> qu'il ne fait que *lister*, faute de preuve de propriété).

## Application (checkpoints — voir CLAUDE.md / SPEC noyau)

1. **Ingestion** (`Node::add`) : un contenu matché est refusé — ni stocké, ni annoncé.
2. **Réception** (`Node::get`) : un contenu matché n'est ni récupéré, ni mis en
   cache, ni reseedé.
3. **Service** (requête entrante) : un nœud refuse de servir un contenu matché.

> Publication d'une denylist signée : outil `champinium-denylist` côté agent INFRA
> (à venir). En attendant, `Denylist::build_signed` (noyau) produit le format.
