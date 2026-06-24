# Denylists

Modération **côté nœud, fédérée** — le seul mécanisme possible sur un réseau
décentralisé où la suppression centrale n'existe pas par construction. Deux
niveaux, tous deux appliqués par le moteur du noyau ([`crate::moderation`]).

## 1. Denylist par défaut — `default.cids` (non désactivable)

- Un CID par ligne (`#` = commentaire). **Compilée dans le binaire** du noyau via
  `include_str!`, donc inaltérable à l'exécution → **non désactivable**.
- Toujours active, quel que soit le front. Vide à ce stade (aucune base de hash
  intégrée par défaut) ; les opérateurs y ajoutent des CIDs puis recompilent.

## 2. Denylists signées souscrites — format `champinium-denylist/v1`

- Modèle **fédéré** : un nœud choisit de suivre une ou plusieurs listes tierces
  (modération subjective). Leur **signature Ed25519 est vérifiée** avant prise en
  compte (`Moderation::subscribe`). Une liste non signée ou altérée est rejetée.
- Format (voir `default-denylist.json` comme gabarit) :

  ```json
  {
    "schema": "champinium-denylist/v1",
    "name": "...",
    "issuer_pubkey": "<clé publique Ed25519, protobuf libp2p, base64>",
    "updated": "<RFC 3339>",
    "entries": ["<cid>", "..."],
    "signature": "<signature Ed25519 base64 des octets canoniques>"
  }
  ```

- Octets signés (déterministes) : `schema \n name \n updated \n` puis les CIDs
  **triés**, séparés par `\n`. La signature ne dépend donc pas de la mise en forme
  JSON.
- Souscription côté CLI : `champinium-cli --denylist liste.json <commande>`.

## Application (checkpoints — voir CLAUDE.md / SPEC noyau)

1. **Ingestion** (`Node::add`) : un contenu matché est refusé — ni stocké, ni annoncé.
2. **Réception** (`Node::get`) : un contenu matché n'est ni récupéré, ni mis en
   cache, ni reseedé.
3. **Service** (requête entrante) : un nœud refuse de servir un contenu matché.

> Publication d'une denylist signée : outil `champinium-denylist` côté agent INFRA
> (à venir). En attendant, `Denylist::build_signed` (noyau) produit le format.
