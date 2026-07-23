# AGENTS.md — Équipe d'agents Champinium

Ce fichier définit l'**équipe de 5 agents**, leurs périmètres, la **règle du
contrat UniFFI**, les garde-fous communs, le protocole de changement de contrat,
et l'ordre de travail par phase. Il complète [`CLAUDE.md`](CLAUDE.md) (vision,
architecture, risques) — à lire en premier.

> **Cette étape ne contient AUCUNE logique métier.** Seulement la structure
> d'équipe, les contrats d'interface et les garde-fous.

## Règle de coordination : LE CONTRAT EST LA FRONTIÈRE UniFFI

La **surface UniFFI** du noyau (fonctions/types annotés `#[uniffi::export]` /
`#[derive(uniffi::*)]` dans `crates/champinium-core`) est le **contrat partagé**.

- Les agents UI codent **CONTRE ce contrat**, jamais contre l'implémentation.
- **Seul l'agent NOYAU** modifie la surface UniFFI. Il la **versionne**
  (`CONTRACT_VERSION` dans `lib.rs`).
- Les agents UI **ne modifient JAMAIS** le noyau. S'ils ont besoin d'une
  capacité absente, ils **ouvrent une demande de changement de contrat** (voir
  protocole plus bas) — ils ne contournent pas via du code natif ad hoc.

### Contrat actuel — v8 (`CONTRACT_VERSION = 8`)

> v1 → v2 : ajout de `subscribe_denylist(json) -> u64` sur `ChampiniumNode`
> (modération fédérée activable depuis les fronts). Purement additif.
>
> v2 → v3 : **`FfiError` typé** (variantes `Moderated`, `Network`, `NotFound`,
> `InvalidInput`, `Internal` — les fronts branchent une UX par catégorie ;
> rupture : l'erreur n'est plus une chaîne aplatie) + **callback interface
> `CatalogListener`** (`on_catalog_updated()`) enregistrée via
> `set_catalog_listener(listener)` (async) — remplace le délai gossip codé en
> dur dans les fronts par un rafraîchissement réactif du catalogue.
>
> v3 → v4 : **recherche décentralisée**. Records `FfiContentItem { cid, title,
> tags }` et `FfiSearchHit { issuer, cid, title, tags }` ; `FfiCatalogEntry`
> gagne `items` (contenus enrichis — rupture pour qui construit le record) ;
> `publish_feed_with(items)` (async — feed v2 à métadonnées signées + annonce
> des tags dans la DHT), `search(query)` (sync — index local, ne couvre que ce
> que le nœud a vu passer), `search_tag(tag)` (async — découverte par tag via
> la DHT, feeds signés vérifiés).
>
> v4 → v5 : **channels**. Record `FfiChannelProfile { name, description,
> avatar_cid }` ; `FfiCatalogEntry` gagne `channel` (rupture pour qui construit
> le record) ; `set_channel_profile(profile)` (async — persiste et republie le
> feed courant), `channel_profile()` (sync). Côté fil : schéma unique
> `champinium-feed/v3` (bloc channel signé), formats v1/v2 supprimés.
>
> v5 → v6 : **abonnements**. Abonnements = **état local privé, jamais publié**
> sur le réseau. `subscribe_channel(link_or_peer_id)` (async — persiste
> immédiatement puis déclenche un fetch immédiat en tâche de fond ; accepte un
> lien `champinium://channel/<clé>` OU un PeerId nu, entrée invalide →
> `InvalidInput`), `unsubscribe_channel(peer_id)` (async), `subscriptions()`
> (sync — PeerIds triés), `catalog_subscribed()` (sync — catalogue restreint
> aux émetteurs souscrits), `channel_link(peer_id)` (sync — pour le bouton
> « copier le lien de mon channel »). Purement additif.
>
> v6 → v7 : **seed proactif — quota, stats, pins** (spec channels lot c,
> surface FFI). `seed_quota() -> u64` (sync), `set_seed_quota(bytes)`
> (async — persiste puis réveille la boucle de seed), `storage_stats() ->
> FfiStorageStats { used_bytes, quota_bytes }` (sync), `pin_content
> (manifest_cid)` / `unpin_content(manifest_cid)` (async, CID invalide →
> `InvalidInput` — épingle/dé-épingle, exempte ou remet évictable sous quota) ;
> `FfiCatalogEntry` gagne `seeded_count`, `total_count` (couverture du seed
> proactif sur le feed courant de l'émetteur) et `pinned` (manifestes épinglés
> de ce feed) — rupture pour qui construit le record. Callback interface
> **`SeedListener`** (`on_seed_updated()`), enregistrée via
> `set_seed_listener(listener)` (async — même patron que `CatalogListener`) :
> notifie tout changement effectif du seed proactif (publication nouvellement
> seedée, éviction, purge au désabonnement).
>
> v7 → v8 : **blocage local de channel** (spec channels lot d, tâche 3).
> `block_channel(link_or_peer_id)` (async — même tolérance d'entrée que
> `subscribe_channel` : lien `champinium://channel/<clé>` OU PeerId nu, entrée
> invalide → `InvalidInput` ; désabonne si souscrit, purge catalogue/SeedIndex/
> blockstore — **pins compris** — de ce qui était attribué à l'émetteur),
> `unblock_channel(peer_id)` (async — retire la préférence ; rien n'est
> retéléchargé automatiquement, le contenu revient naturellement au prochain
> feed reçu), `blocked_channels() -> Vec<String>` (sync — PeerIds triés). Le
> blocage local est un **état strictement privé de ce nœud, jamais publié** sur
> le réseau (aucun rapport, aucun record signé — contrairement au blocage par
> denylist fédérée, qui reste un choix partagé). `subscribe_channel` visant un
> émetteur bloqué (localement OU par denylist) renvoie désormais
> `FfiError::Moderated` (et non plus `InvalidInput` — correction du mapping
> `CoreError::Moderated → FfiError::Moderated`, distinct de
> `CoreError::Moderation`, réservée aux erreurs de format/signature des
> denylists elles-mêmes). Purement additif côté surface (la correction du
> mapping d'erreur est un changement de comportement, pas de signature).

Fonctions libres (smoke test async, conservées de v0) :

| Fonction (Rust) | Swift | C# | Sémantique |
|---|---|---|---|
| `core_version() -> String` | `coreVersion()` | `CoreVersion()` | version du paquet noyau |
| `contract_version() -> u32` | `contractVersion()` | `ContractVersion()` | version de la surface de contrat |
| `core_handshake(client) -> String` (**async**) | `coreHandshake(_:) async` | `CoreHandshake(...)` | coup de sonde async (risque #1) |
| `open_node(data_dir) -> ChampiniumNode` (**async**) | `openNode(_:) async` | `OpenNode(...)` | ouvre/crée un nœud (modération par défaut active) |

Objet **`ChampiniumNode`** (méthodes) :

| Méthode (Rust) | Type | Sémantique |
|---|---|---|
| `peer_id() -> String` | sync | PeerId du nœud |
| `catalog() -> Vec<FfiCatalogEntry>` | sync | catalogue reconstruit (instantané) |
| `listen(addr) -> String` | **async** | écoute, renvoie l'adresse liée |
| `connect(peer) -> ()` | **async** | se connecte à `/…/p2p/<id>` |
| `ingest_file(path) -> String` | **async** | ffmpeg → HLS, renvoie le CID du manifeste |
| `publish_feed(cids) -> ()` | **async** | publie un feed signé |
| `fetch_hls(manifest_cid, out_dir) -> String` | **async** | reconstruit un HLS jouable, renvoie le playlist |
| `subscribe_denylist(json) -> u64` | **async** | souscrit une denylist signée, renvoie le nb de blocs purgés |
| `set_catalog_listener(listener) -> ()` | **async** | enregistre un `CatalogListener` (rafraîchissement réactif) |
| `publish_feed_with(items) -> ()` | **async** | publie un feed v2 (titre/tags signés) + annonce les tags DHT |
| `search(query) -> Vec<FfiSearchHit>` | sync | recherche locale (titres/tags du catalogue reconstruit) |
| `search_tag(tag) -> Vec<FfiSearchHit>` | **async** | découverte par tag via la DHT (hors gossip) |
| `set_channel_profile(profile) -> ()` | **async** | définit le profil de channel : persiste et republie le feed courant |
| `channel_profile() -> FfiChannelProfile` | sync | profil de channel courant de ce nœud |
| `subscribe_channel(link_or_peer_id) -> ()` | **async** | s'abonne (lien ou PeerId nu) : persiste + fetch immédiat en tâche de fond |
| `unsubscribe_channel(peer_id) -> ()` | **async** | se désabonne d'un émetteur |
| `subscriptions() -> Vec<String>` | sync | abonnements courants (PeerIds triés) |
| `catalog_subscribed() -> Vec<FfiCatalogEntry>` | sync | catalogue restreint aux émetteurs souscrits |
| `channel_link(peer_id) -> String` | sync | lien partageable `champinium://channel/<peerid>` |
| `seed_quota() -> u64` | sync | quota de seed courant (octets) |
| `storage_stats() -> FfiStorageStats` | sync | `(used_bytes, quota_bytes)` du seed proactif |
| `set_seed_quota(bytes) -> ()` | **async** | définit le quota, persiste, réveille la boucle de seed |
| `pin_content(manifest_cid) -> ()` | **async** | épingle un manifeste (jamais évincé sous quota) |
| `unpin_content(manifest_cid) -> ()` | **async** | retire l'épinglage (redevient évictable) |
| `set_seed_listener(listener) -> ()` | **async** | enregistre un `SeedListener` (rafraîchissement réactif du seed) |
| `block_channel(link_or_peer_id) -> ()` | **async** | bloque un émetteur localement (lien ou PeerId nu) : désabonne, purge catalogue/SeedIndex/blockstore (pins compris) |
| `unblock_channel(peer_id) -> ()` | **async** | débloque un émetteur ; rien de retéléchargé automatiquement |
| `blocked_channels() -> Vec<String>` | sync | channels bloqués localement (PeerIds triés) |

Records `FfiCatalogEntry { issuer, seq, cids, items, channel, seeded_count,
total_count, pinned }`, `FfiContentItem { cid, title, tags }`,
`FfiSearchHit { issuer, cid, title, tags }`,
`FfiChannelProfile { name, description, avatar_cid }`,
`FfiStorageStats { used_bytes, quota_bytes }`.
Erreur **`FfiError` typée**
(`Moderated` / `Network` / `NotFound` / `InvalidInput` / `Internal`, chacune avec
`msg`). Callback interfaces **`CatalogListener`** (`on_catalog_updated()`) et
**`SeedListener`** (`on_seed_updated()`) : rappelées hors du thread UI, le front
re-dispatche puis relit `catalog()` / `storage_stats()`.
Validé : bindings Swift **et** C# générés pour toute cette surface (objet + async).

## Mapping périmètre d'agent → répertoires du repo

| Agent | Répertoires possédés | SPEC |
|---|---|---|
| **NOYAU** (`rust-core`) | `crates/champinium-core/`, `crates/champinium-cli/` | [`crates/champinium-core/SPEC.md`](crates/champinium-core/SPEC.md) |
| **macOS** | `apps/macos/` | [`apps/macos/SPEC.md`](apps/macos/SPEC.md) |
| **Windows** | `apps/windows/` | [`apps/windows/SPEC.md`](apps/windows/SPEC.md) |
| **Linux** | `apps/linux/` | [`apps/linux/SPEC.md`](apps/linux/SPEC.md) |
| **INFRA & BUILD** | `infra/`, `deny/`, `justfile`, CI, packaging | [`infra/SPEC.md`](infra/SPEC.md) |

> Note : le périmètre « rust-core » du plan correspond au crate
> `crates/champinium-core` (le crate n'est pas renommé pour éviter de casser le
> câblage du workspace). `bootstrap`/`relay` vivent sous `infra/`.

## Les 5 agents

1. **AGENT NOYAU** — P2P (libp2p), DHT, stockage content-addressed, feeds signés,
   catalogue CRDT, orchestration ffmpeg, moteur de modération, identité/clés.
   **SEUL propriétaire de la surface UniFFI** ; définit et versionne le contrat.
   Responsable des tests du noyau et du prototype async-FFI précoce.
2. **AGENT macOS** — SwiftUI ; consomme les bindings Swift générés ; lecture
   AVPlayer ; agent **launchd** pour le seeding hors UI.
3. **AGENT Windows** — C#/WinUI 3 ; consomme `uniffi-bindgen-cs` ; lecture Media
   Foundation ; **Windows Service** pour le seeding hors UI.
4. **AGENT Linux** — GTK4/gtk-rs ; consomme le crate **directement** (pas de
   FFI) ; lecture GStreamer ; **systemd user service** pour le seeding hors UI.
5. **AGENT INFRA & BUILD** — bootstrap & relay **stateless** (+ doc « lance le
   tien ») ; format/publication des **denylists signées** + denylist par défaut ;
   orchestration du build (justfile : noyau → 3 jeux de bindings → 3 apps) ; CI
   multi-OS ; packaging/signature (doc).

## Garde-fous communs à TOUS les agents

- **Modération par défaut, non contournable.** Aucun agent ne produit de chemin
  de code qui contourne le hash-matching à l'ingestion ou le respect des
  denylists. **Le reseed d'un contenu matché est interdit par design.**
- **Décentralisé / stateless.** Aucun agent n'introduit de dépendance à un
  service central obligatoire au fonctionnement.
- **Frontière de contrat.** Un agent UI qui a besoin d'une nouvelle capacité
  **ouvre une demande de changement de contrat** à l'agent NOYAU ; il ne
  contourne pas via du code natif ad hoc.
- **Commits atomiques préfixés par périmètre** : `core:`, `macos:`, `win:`,
  `linux:`, `infra:`.

## Protocole de changement de contrat (versionné)

1. **Demande.** L'agent UI décrit la capacité nécessaire (signature souhaitée,
   types, sync/async) dans sa PR/issue, préfixe `contract-request:`.
2. **Conception.** L'agent NOYAU conçoit la signature, l'implémente derrière
   `#[uniffi::export]`, **incrémente `CONTRACT_VERSION`**, et ajoute une entrée
   au **tableau du contrat ci-dessus** dans ce fichier (le contrat vit ici).
3. **Annonce.** Le commit du noyau est préfixé `core: contract vN -> vN+1` et
   liste les fonctions ajoutées/modifiées/supprimées.
4. **Régénération.** L'agent INFRA régénère les 3 jeux de bindings (`just
   gen-bindings`) ; les agents UI mettent à jour leur code contre la nouvelle
   surface.
5. **Compat.** Les fronts peuvent vérifier `contract_version()` au démarrage
   pour détecter une incompatibilité binaire.

Les bindings générés ne sont **jamais commités** (gitignorés) : la surface Rust
+ ce tableau sont la seule source de vérité.

## Ordre de travail recommandé par phase

**Principe : le noyau et le contrat d'abord ; les fronts ensuite contre des
bindings générés ; l'infra en parallèle.**

- **Phase 0 (de-risking async FFI)** — NOYAU étend le contrat avec une vraie
  `async fn` + un `Stream<Event>`. INFRA câble la génération des 3 bindings dans
  le justfile/CI. Les 3 agents UI consomment le stub et affichent l'événement.
  *Sortie : un événement tokio apparaît dans les 3 fronts.*
- **Phase 1 (P2P nu)** — NOYAU (Swarm libp2p, blockstore CID, identité) +
  `champinium-cli`. INFRA : bootstrap stateless. Fronts en veille (le contrat
  P2P n'est pas encore exposé en UI).
- **Phase 2 (publication signée)** — NOYAU (ffmpeg→HLS, feeds signés, CRDT,
  checkpoint modération #1) ; INFRA : format denylist + denylist par défaut.
- **Phase 3 (MVP macOS)** — NOYAU expose le contrat catalogue+lecture ; agent
  **macOS** construit l'UI (catalogue + AVPlayer), checkpoint #2 actif.
- **Phase 4 (parité 3 OS + NAT)** — agents **Windows** & **Linux** en parallèle
  contre le même contrat ; INFRA : relay + seeding par OS (launchd/service/systemd).
- **Phases 5-6** — robustesse/modération fédérée (NOYAU+INFRA) ; packaging/
  signature (INFRA).
