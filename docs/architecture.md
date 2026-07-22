# Architecture de Champinium — le guide complet

> Public : quiconque veut comprendre comment le projet fonctionne de bout en
> bout. Les renvois pointent vers le code (chemins cliquables) et les ADRs
> (`docs/adr/`) pour les décisions. État au contrat FFI **v5** / release
> **v0.6.x**.

## 1. Ce que c'est, en une phrase

Une plateforme de partage P2P de contenu généré par IA (vidéo d'abord), avec
l'UX de Popcorn Time (parcourir → cliquer → ça joue) et l'architecture
inverse : **native sur les 3 OS** (pas d'Electron) et **décentralisée jusque
dans la découverte** (pas d'API centrale, pas de base serveur, pas de stockage
qu'on possède).

Deux principes non négociables en découlent :

1. **Natif intégral ×3** — macOS = SwiftUI, Windows = WinUI 3, Linux = GTK4.
2. **Décentralisé & stateless au maximum** — tout état vit dans le réseau ou
   dans le cache local de chaque nœud. Les seules pièces « d'infrastructure »
   (bootstrap, relay) sont **sans état** et multipliables par n'importe qui.

## 2. Topologie : qui parle à qui

```
        ┌────────────┐  rendez-vous DHT   ┌────────────┐
        │ BOOTSTRAP  │◄———————————————————│   nœud A   │ (créateur)
        │ (stateless)│                    │ cli/app    │
        └────────────┘                    └─────┬──────┘
                                                │ gossipsub (feeds, rapports)
        ┌────────────┐   circuit + DCUtR       │ Kademlia (providers, records)
        │   RELAY    │◄———————┐                │ request-response (blocs)
        │ (stateless)│        │          ┌─────▼──────┐
        └────────────┘   nœud derrière   │   nœud B   │ (spectateur/seeder)
                         NAT             │  app GUI   │
                                         └────────────┘
```

- Chaque application (GUI ou CLI) embarque un **nœud complet** : il stocke,
  sert, découvre et modère. Il n'y a pas de « client » et de « serveur ».
- Le **bootstrap** ([`infra/bootstrap`](../infra/bootstrap)) n'est qu'un point
  de rendez-vous Kademlia stable (PeerId persisté) pour découvrir ses premiers
  pairs. Il ne stocke aucun bloc.
- Le **relay** ([`infra/relay`](../infra/relay)) fait de la mise en relation
  NAT (circuit relay v2) et assiste le hole punching (DCUtR) ; il ne voit pas
  le contenu en clair. Guide opérateur : [`deploy-bootstrap-relay.md`](deploy-bootstrap-relay.md).

## 3. Le noyau Rust : toute la logique, un seul endroit

**`crates/champinium-core`** contient 100 % de la logique. Les fronts ne font
que de la présentation (c'est un garde-fou d'architecture : de la logique dans
un front est un bug). Carte des modules :

| Module | Rôle |
|---|---|
| [`content`](../crates/champinium-core/src/content.rs) | CIDv1 (raw/sha2-256, compatible IPFS), vérification octets↔CID, `push_field` (encodage signé anti-malléabilité, §5) |
| [`blockstore`](../crates/champinium-core/src/blockstore.rs) | stockage content-addressed sur disque : un fichier par CID, **écriture atomique** (tempfile+persist), intégrité vérifiée à la lecture, `remove` pour la purge de modération |
| [`identity`](../crates/champinium-core/src/identity.rs) | paire Ed25519 persistée (`node.key`, mode 0600) → PeerId |
| [`feed`](../crates/champinium-core/src/feed.rs) | feed signé d'un créateur, versionné par `seq` (LWW) ; **v3** = métadonnées titre/tags par entrée + identité de channel (nom/description/avatar) signées, v1/v2 supprimés |
| [`catalog`](../crates/champinium-core/src/catalog.rs) | CRDT maison : map last-writer-wins par émetteur, bornée (1024 émetteurs) ; recherche locale |
| [`moderation`](../crates/champinium-core/src/moderation.rs) | denylist compilée (non désactivable) + denylists signées souscrites (fédéré) |
| [`report`](../crates/champinium-core/src/report.rs) | signalement P2P : rapport signé + agrégateur borné de rapporteurs distincts |
| [`ingest`](../crates/champinium-core/src/ingest.rs) | orchestration ffmpeg → segments HLS alignés keyframes → manifeste `champinium-hls/v1` |
| [`p2p`](../crates/champinium-core/src/p2p.rs) | le cœur : `Node`, la boucle d'évènements libp2p, tous les flux (§4, §6) |
| [`relay`](../crates/champinium-core/src/relay.rs) | serveur de relais stateless (utilisé par `infra/relay`) |
| [`paths`](../crates/champinium-core/src/paths.rs) | répertoire de données durable par OS |
| [`ffi`](../crates/champinium-core/src/ffi.rs) | la surface UniFFI = **le contrat** avec les fronts (§8) |
| [`error`](../crates/champinium-core/src/error.rs) | `CoreError` (thiserror) — mappé en erreurs typées côté FFI |

Consommateurs du noyau : [`champinium-cli`](../crates/champinium-cli) (outil
créateur/debug), [`champinium-seed`](../crates/champinium-seed) (démon de
seeding), les trois fronts, et les binaires d'infra.

## 4. Modèle d'exécution : un handle, une boucle

Le point subtil du code : `Node` n'est **pas** le swarm libp2p. C'est un
handle clonable qui parle à une **boucle d'évènements** tournant dans sa
propre tâche tokio :

```
   Node (handle, Clone)                    EventLoop (tâche tokio dédiée)
   ├─ blockstore ────────┐                 ┌──────────────────────────────┐
   ├─ catalog (Mutex) ───┼── partagés ────►│  tokio::select! {            │
   ├─ moderation (RwLock)┘                 │    cmd  = cmd_rx.recv()      │
   ├─ catalog_events (broadcast ◄──────────│    event= swarm.next()       │
   ├─ reports (Mutex)                      │  }                           │
   └─ cmd_tx ────── mpsc<Command> ────────►│  Swarm<Behaviour>            │
        Listen/Dial/Provide/GetProviders/  │   kademlia · gossipsub ·     │
        RequestBlock/PublishFeed/PutRecord/│   request-response(blocs) ·  │
        GetRecord/PeerScore/PublishReport  │   identify · ping ·          │
                                           │   relay-client · dcutr       │
                                           └──────────────────────────────┘
```

- Toute opération réseau = un `Command` envoyé avec un canal `oneshot` de
  réponse ; la boucle corrèle les réponses asynchrones du swarm (par
  `QueryId`, `ListenerId`, `OutboundRequestId`) vers le bon appelant.
- L'état applicatif (catalogue, modération, rapports) est **partagé** entre le
  handle et la boucle via `Arc<Mutex/RwLock>` — la boucle l'alimente depuis le
  réseau, le handle le lit instantanément (les getters `catalog_entries`,
  `search`, `report_count` sont synchrones).
- Le canal broadcast `catalog_events` émet un **tic fusionnable** à chaque
  changement effectif du catalogue — c'est ce qui rend les UI réactives (§8).

Comportements libp2p et leurs rôles :

| Behaviour | Sert à |
|---|---|
| **Kademlia** (mode serveur) | provider records (« qui détient quel CID / quel tag ») + records de feed `/champinium/feed/<peerid>` ; stores **bornés** et **filtrés** (un record entrant est validé avant stockage) |
| **gossipsub** | topics `champinium/feeds/v1` et `champinium/reports/v1`, messages signés ; **validation applicative** (un message n'est relayé qu'après verdict Accept) + **peer scoring** (un émetteur d'invalide est pénalisé puis graylisté) |
| **request-response** (cbor) | transfert de blocs `/champinium/block/1.0.0` (interim — bitswap différé, ADR 0006), plafonds 64 MiB/bloc |
| **relay-client + DCUtR** | écouter/joindre via un relais et tenter le direct (NAT) |
| **identify / ping** | peuplement de la table de routage / liveness |

## 5. Les données et leurs formats

Tous les objets qui circulent sont **signés Ed25519** et leurs octets signés
utilisent un encodage **préfixé par longueur** (`push_field`) : impossible de
déplacer du contenu d'un champ à l'autre à signature constante
(anti-malléabilité, testé).

- **Bloc** : des octets adressés par leur **CID** (CIDv1 raw/sha2-256). Le CID
  EST la vérité : tout bloc reçu du réseau est vérifié contre le CID demandé
  avant usage. Un « contenu » vidéo = 1 manifeste + N segments, chacun un bloc.
- **Manifeste HLS** (`champinium-hls/v1`, JSON) : ordre + durée des segments →
  leurs CIDs. `fetch_hls` le retransforme en `index.m3u8` jouable localement.
- **Feed** (`champinium-feed/v3`, JSON signé) : LA publication d'un créateur.
  `{schema, issuer_pubkey, seq, channel{name, description, avatar_cid},
  entries[{cid, title, tags}], signature}`. Versionné par `seq` **monotone et
  persisté** (`.feed_seq` à côté des blocs : un créateur qui redémarre ne
  régresse pas, sinon le LWW des pairs l'ignorerait). Bornes vérifiées à la
  réception (titre ≤ 256, ≤ 16 tags). Le bloc `channel` porte l'identité
  éditoriale du créateur, signée avec le reste du feed (pas de canal séparé
  ni de confiance implicite) : nom ≤ 64, description ≤ 1024. `avatar_cid` est
  optionnel et, s'il est présent, doit être un CID valide — **c'est un CID
  comme un autre** : il traverse les mêmes checkpoints de modération que
  n'importe quel contenu (pas de contournement pour les avatars). Le profil
  courant est persisté par le créateur (`.channel_profile`, à côté du feed) et
  republié (nouveau `seq`) à chaque changement. Les formats **v1 et v2 ont été
  supprimés** (zéro utilisateur en usage réel au moment du retrait) : un feed
  v1/v2 reçu est rejeté au parsing plutôt que toléré en compatibilité
  descendante.
- **Denylist** (`champinium-denylist/v1`, JSON signé) : liste de CIDs bloqués,
  signée par son éditeur. Voir [`deny/README.md`](../deny/README.md).
- **Rapport** (`champinium-report/v1`, JSON signé) : « ce CID a été refusé par
  ma modération » — voir §7.

## 6. Les flux, de bout en bout

### Publication (créateur)

```
fichier → [modération #1] → ffmpeg (segments HLS) → blocs CID + manifeste
       → feed v3 signé { seq+1, channel, entries }
            ├─► catalogue local (+ tic catalog_events)
            ├─► gossipsub feeds/v1                     (live, secondes)
            ├─► DHT PUT /champinium/feed/<peerid>      (découverte hors gossip)
            └─► DHT provide /champinium/tag/<tag>      (découverte par tag)
       + provider records pour chaque bloc (« je détiens »)
```

### Découverte (spectateur) — trois chemins

1. **Gossip** : abonné au topic, le nœud vérifie chaque feed (signature,
   bornes) et l'applique au catalogue LWW → l'UI est notifiée par le tic.
2. **DHT par créateur** : `fetch_feed(peerid)` fait un GET du record, vérifie
   signature **et** correspondance émetteur↔clé (un tiers ne peut pas écraser
   le feed d'autrui : les nœuds stockeurs filtrent aussi à l'entrée).
3. **DHT par tag** : `search_tag("nature")` → providers de
   `/champinium/tag/nature` → ce sont les émetteurs eux-mêmes → `fetch_feed`
   de chacun, filtrage par tag. Un tag annoncé à tort ne coûte qu'une requête
   (le feed signé fait foi). La recherche **locale** (`search`) ne parcourt
   que le catalogue déjà reconstruit — limite assumée : pas de recherche
   globale exhaustive sur un réseau décentralisé (risque #4).

### Lecture + seed-what-you-consume

```
clic « Lire » → fetch_hls(manifeste)
  get(manifeste) : cache local ? sinon [modération #2] → providers DHT
      → requêtes à TOUS les fournisseurs EN PARALLÈLE, 1ʳᵉ réponse valide gagne
      → bloc vérifié contre le CID → cache local → REPROVIDE (je deviens fournisseur)
  idem pour chaque segment → écrit index.m3u8 + .ts → lecteur natif
  (échec en route → répertoire de sortie nettoyé, pas de lecture partielle)
```

Consommer, c'est répliquer : c'est la mitigation n°1 du risque « un contenu
sans seeder disparaît ».

### Persistance active

- **`champinium-seed`** (démon, fichiers de service dans
  [`infra/services/`](../infra/services)) : réannonce périodiquement tous les
  CIDs détenus (le store de providers Kademlia est volatil), republie le feed,
  et fait de la **réplication opportuniste** : tout contenu du catalogue ayant
  moins de N fournisseurs (`replication_factor`, défaut cible 2) est répliqué
  — manifeste **et** segments, bornes par passe.
- Un bloc local **corrompu** (crash pendant écriture) n'est pas fatal : `get`
  détecte l'incohérence d'intégrité et re-télécharge du réseau, ce qui répare
  le fichier.

## 7. Modération : le garde-fou obligatoire

La suppression centrale est impossible par construction → la modération est
**côté nœud, active par défaut, non désactivable**, en trois checkpoints qui
passent tous par le même moteur :

1. **Ingestion** (`add`) : un CID matché n'est ni stocké ni annoncé.
2. **Réception** (`get`) : ni récupéré, ni mis en cache, ni reseedé — et le
   refus émet un **rapport signé** sur `champinium/reports/v1` (best-effort).
3. **Service** (requête entrante de bloc) : jamais servi.

Sources de blocage : la denylist **compilée dans le binaire**
(`deny/default.cids`, inaltérable à l'exécution) + les denylists **signées
souscrites** (modèle fédéré : chaque nœud choisit qui suivre ; signature
vérifiée ; souscription à chaud possible avec **purge rétroactive** du cache).

Autour, deux mécanismes d'écosystème :

- **Signalement** : les pairs agrègent les rapports par CID (rapporteurs
  **distincts**, agrégat borné) — matière première pour les éditeurs de
  denylists, **aucun effet automatique** sur le contenu.
- **Peer scoring gossipsub** : émettre des feeds/rapports invalides dégrade le
  score du pair → ses messages ne sont plus relayés → graylist. Avec le
  catalogue borné à 1024 émetteurs (refus-quand-plein, pas d'éviction), c'est
  la défense contre l'inondation par clés jetables.

## 8. La frontière FFI : le contrat v5

La surface UniFFI de [`ffi.rs`](../crates/champinium-core/src/ffi.rs) est
**le contrat** entre le noyau et les fronts (tableau exhaustif et protocole de
changement dans [`AGENTS.md`](../AGENTS.md)). Ce qui la caractérise :

- **Async de bout en bout** : les méthodes réseau sont des `async fn` tokio
  exposées telles quelles à Swift (`await`) et C# (`Task`) — le risque
  technique n°1 du projet, éprouvé depuis la Phase 0.
- **Événements par callback** : `CatalogListener.on_catalog_updated()` est
  rappelé (hors thread UI) à chaque changement du catalogue ; le front
  re-dispatche vers son thread principal et relit l'instantané `catalog()`.
  Choix assumé vs un `Stream` : tic fusionnable + relecture d'instantané, plus
  simple des deux côtés.
- **Erreurs typées** : `FfiError::{Moderated, Network, NotFound, InvalidInput,
  Internal}` — un contenu bloqué par la modération s'affiche « contenu
  bloqué », pas comme une panne réseau.
- **Bindings générés au build, jamais commités** : Swift via
  UniFFI/XCFramework (`just macos-prepare`), C# via `uniffi-bindgen-cs`
  (`just gen-csharp`). Le front Linux consomme le crate **directement** (pas
  de FFI). `CONTRACT_VERSION` (=5) permet aux fronts de détecter une
  incompatibilité au démarrage.

Les trois fronts ([`apps/macos`](../apps/macos),
[`apps/windows`](../apps/windows), [`apps/linux`](../apps/linux)) font tous la
même chose et rien d'autre : ouvrir le nœud (répertoire durable par OS),
écouter, se connecter, afficher le catalogue (réactif), chercher, lire avec le
lecteur natif (AVPlayer / MediaPlayerElement / GStreamer), nettoyer leurs
répertoires de lecture temporaires.

## 9. Cycle de vie local d'un nœud

```
<data-dir>/                    (paths::default_data_dir(), durable par OS)
├── node.key                   identité Ed25519 (0600) → PeerId stable
└── blocks/
    ├── <cid>                  un fichier par bloc (écrit atomiquement)
    └── .feed_seq              dernier seq de feed publié (LWW au redémarrage)
```

Le catalogue, les scores gossip, les providers DHT et l'agrégat de rapports
sont **volatils** (reconstruits par écoute / réannonce). C'est voulu : l'état
qui compte vit dans le réseau, chaque nœud n'en garde qu'une vue.

## 10. Build, CI, releases

- **Build** : `just` orchestre ([`justfile`](../justfile)) — `check`
  (fmt+clippy strict+tests), `gen-swift`/`gen-csharp`, `macos-build`,
  `macos-app`.
- **CI** ([`ci.yml`](../.github/workflows/ci.yml)) : commits conventionnels,
  fmt, tests sur les 3 OS, + trois jobs de fronts (`linux-gui`, `macos-app`,
  `windows-app`) — les fronts non compilables en local sont validés là.
- **Releases** : release-please (pré-1.0 : un breaking change bumpe la
  **mineure**) ; à chaque release publiée, le workflow
  [`release-artifacts.yml`](../.github/workflows/release-artifacts.yml)
  attache les binaires (apps 3 OS + outils), **sans coût de signature**
  (palier gratuit — voir [`packaging.md`](packaging.md)).
- **Gouvernance** : `.intendant.toml` (subprojects rust + swift), audit 100.

## 11. Limites connues et où en sont les décisions

| Sujet | État | Référence |
|---|---|---|
| bitswap | différé (amont cassé ; le fetch multi-fournisseurs couvre le bénéfice pratique) — débloquable par un bump `multihash-codetable` côté beetswap | ADR 0006 |
| IPNS | différé, adossé à bitswap (sa valeur = interop IPFS public) ; durabilité déjà couverte par la republication du seed | ADR 0007 |
| Recherche | locale (ce que le nœud a vu) + tags DHT ; pas d'index global — assumé | risque #4, §6 |
| Persistance | seed-what-you-consume + réplication opportuniste ; cold storage (Filecoin/Arweave) documenté non implémenté | risque #1 |
| NAT | relay v2 + DCUtR testés ; relays multipliables | risque #6 |
| Signature payante | palier gratuit livré ; notarisation/Authenticode différés | `packaging.md` |
| Windows/C# | validé par CI ; pas de stack intendant | `.intendant.toml` |

## 12. Carte des documents

- [`CLAUDE.md`](../CLAUDE.md) — principes + état d'avancement (source de vérité).
- [`AGENTS.md`](../AGENTS.md) — contrat FFI (tableau v5) + garde-fous d'équipe.
- [`docs/adr/`](adr/) — décisions : libp2p vs iroh (0001), modération côté
  nœud (0002), feeds signés (0003), transport de blocs (0006), IPNS (0007)…
- [`docs/mvp-demo.md`](mvp-demo.md) / [`docs/gui-demo.md`](gui-demo.md) —
  démos de bout en bout (CLI validée ; GUI deux machines à dérouler).
- [`docs/deploy-bootstrap-relay.md`](deploy-bootstrap-relay.md) — opérer
  l'infra stateless.
- [`docs/packaging.md`](packaging.md) — distribution sans frais de signature.
