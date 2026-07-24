# CLAUDE.md — Champinium

Plateforme de partage P2P de contenu généré par IA (vidéo, image, audio).
UX esprit Popcorn Time ; architecture à l'opposé : **natif, pas Electron ;
décentralisé jusque dans la découverte, pas d'API centrale.**

> Spec d'architecture complet : `~/Work/.superpowers/champinium/specs/2026-06-24-bootstrap-architecture.md`
> (hors repo — artefact de design local).
> Équipe d'agents, contrat UniFFI et garde-fous : [`AGENTS.md`](AGENTS.md).

## Principes directeurs NON négociables

1. **Natif intégral, 3 OS.** Pas de webview / Electron / Tauri.
   macOS = Swift/SwiftUI · Windows = C#/WinUI 3 · Linux = GTK4 (gtk-rs).
2. **Décentralisé & stateless au maximum.** Aucun composant central obligatoire :
   pas de DB serveur, pas d'API stateful, pas de stockage qu'on possède. Tout état
   vit dans le réseau ou en cache local. Bootstrap & relay résiduels = SANS ÉTAT,
   multipliables trivialement par n'importe qui.

## Architecture : noyau Rust partagé + 3 fronts natifs

Toute la logique vit dans **`crates/champinium-core`** (Rust, tokio/libp2p),
exposée aux UI via **UniFFI**. Le ×3 ne touche QUE la présentation — **aucune
logique métier dans les fronts.**

- **P2P** : rust-libp2p (TCP/QUIC) — Kademlia (provider records), gossipsub,
  bitswap, relay-v2 + DCUtR, identify/ping.
- **Discovery** : provider records Kademlia (qui détient quel CID).
- **Feeds** : records gossip signés (primaire) + IPNS durable (différé post-MVP).
- **Catalogue** : CRDT *maison* reconstruit localement par écoute gossipsub.
- **Stockage** : content-addressed (CID) + cache LRU local.
- **Identité** : Ed25519 → PeerID/DID. Tout contenu et tout feed est SIGNÉ.
- **Lecture** : native par OS — AVPlayer / Media Foundation / GStreamer (pas de hls.js).
- **Modération** : moteur côté nœud, deux checkpoints, denylists signées (voir plus bas).

## Bindings

- Source unique de vérité = `champinium-core`. Bindings **générés au build**
  (jamais commités, voir `.gitignore`).
- Swift : UniFFI → XCFramework. C# : `uniffi-bindgen-cs`. Linux/GTK : consomme le
  crate Rust directement (pas de FFI).
- ⚠️ **Async via FFI = risque technique #1.** Toute fonction async ou stream
  d'événements exposée doit être prototypée et testée TÔT vers Swift ET C#.

## Conventions par langage

- **Rust (core, cli, bootstrap, relay, GTK)** : edition 2021, `cargo fmt` +
  `cargo clippy -- -D warnings` propres avant commit. Erreurs via `thiserror`/
  `anyhow` (jamais d'`unwrap()` sur chemin réseau). Async = tokio. Types exposés
  via UniFFI annotés `#[uniffi::export]` / `#[derive(uniffi::*)]`.
- **Swift (macOS)** : SwiftUI, pas de logique métier (délègue au core). `swiftformat` (config apps/macos/.swiftformat, lint en CI).
- **C# (Windows)** : WinUI 3, MVVM léger, async/await sur les fns du core.
- **Tout front** : si tu écris de la logique non-présentation dans un front,
  c'est un bug d'architecture — remonte-la dans le core.

## Modération — garde-fou OBLIGATOIRE (ne pas désactiver)

La suppression centrale est impossible par construction → modération côté nœud,
**active par défaut** :
- **Checkpoint #1 (ingestion locale)** : hash-match vs bases connues → refus si match.
- **Checkpoint #2 (réception, AVANT tout reseed)** : hash-match + denylists signées
  souscrites → DROP, pas de reseed, signalement P2P. S'applique quelle que soit la
  source (l'interop IPFS public expose à du contenu de pairs non-Champinium).
- Denylist par défaut active à l'installation (`deny/`). Modèle fédéré (subjectif
  par nœud), format inspiré des denylists IPFS.

## Risques classés

1. **Persistance** — contenu sans seeder disparaît. Mitigation : seed proactif
   des abonnés (chaque abonné retient et resert ce qu'il suit, sous quota) +
   pins (contenu propre auto-épinglé, plus tout manifeste épinglé manuellement) ;
   cold storage optionnel Arweave (payé par le créateur — ADR 0008) livré côté
   cœur+CLI (CS-a) derrière la feature opt-in `cold-storage`, repli de dernier
   recours CID-vérifié — voir « État actuel ».
2. **Async FFI** — async/streams tokio → Swift ET C#. Mitigé par le spike Phase 0.
3. **Modération décentralisée** — deux checkpoints, denylists signées.
4. **Recherche décentralisée non résolue** — tags DHT + index local ; limites assumées.
5. **Coûts vidéo** — cold storage opt-in livré (CS-a, feature `cold-storage`),
   archivage par publication choisie et non « tout mon channel », devis affiché.
6. **NAT traversal** — relay-v2 + DCUtR ; relays stateless multipliables.
7. **Signature multi-OS** — notarisation Apple / Authenticode / Flatpak (Phase 6).
8. **Maintenance ×3 UI** — mitigée par zéro logique dans les fronts.

## Structure de répertoires

```
crates/champinium-core/   noyau Rust partagé (UniFFI) — TOUTE la logique
crates/champinium-cli/    outil debug (Phase 1+)
infra/bootstrap/          nœud rendez-vous stateless
infra/relay/              relay NAT stateless
apps/macos/               SwiftUI (SwiftPM/Xcode)
apps/windows/             WinUI 3 (.sln, C#)
apps/linux/               GTK4 (gtk-rs)
bindings/                 généré au build (gitignoré)
deny/                     denylist par défaut signée
docs/                     documentation
```

## Build

`just` orchestre tout. `just build-core` compile le noyau ; `just gen-swift` /
`just gen-csharp` régénèrent les bindings ; `just check` lance fmt+clippy+tests.
Voir le `justfile` à la racine.

## État actuel

**Phase 1 (noyau P2P nu) — faite.** Le noyau implémente : CID content-addressed
(CIDv1 raw/sha2-256, compatible IPFS), blockstore sur disque avec vérification
d'intégrité, identité Ed25519 persistée, et un nœud libp2p (TCP/Noise/Yamux) avec
Kademlia (provider records), identify, ping et un protocole request-response
`/champinium/block/1.0.0` pour le transfert de blocs. `champinium-cli` et le nœud
`champinium-bootstrap` stateless pilotent ce noyau. Démo deux nœuds : voir
[`docs/phase1-demo.md`](docs/phase1-demo.md).

> Note transfert : Phase 1 utilise **request-response** comme transport de blocs
> (interim) ; le passage à **bitswap** est prévu pour une phase ultérieure.

**Phase 2 — en cours.**
- **Modération ✔** (faite en premier) : moteur `moderation` — denylist par défaut
  compilée dans le binaire (non désactivable) + denylists signées Ed25519
  souscrites (modèle fédéré, signature vérifiée). Enforcement aux trois points :
  ingestion (`add`), réception (`get`), service (requête entrante). CLI :
  `--denylist <fichier>`. Voir [`deny/README.md`](deny/README.md).
- **Feeds signés + gossipsub + catalogue ✔** : `feed` (record `champinium-feed/v1`
  signé Ed25519, versionné par `seq`), diffusé en **gossipsub** ; `catalog` (CRDT
  maison last-writer-wins par émetteur) reconstruit en écoutant. Node :
  `publish_feed` / `catalog_entries`. CLI : `catalog --peer …`. Le `seq` est
  **persisté** (à côté des blocs) : un créateur qui redémarre reprend son `seq`,
  sinon ses nouveaux feeds seraient ignorés par le LWW des pairs.
- **Ingestion ffmpeg → HLS ✔** : `ingest` orchestre ffmpeg (segments alignés sur
  keyframes), chaque segment = un bloc CID (checkpoint #1 via `add`), un manifeste
  `champinium-hls/v1` mappe l'ordre/durée aux CIDs. `Node::ingest_file` →
  CID du manifeste ; `Node::fetch_hls` reconstruit un `index.m3u8` jouable. CLI :
  `ingest <fichier>` / `fetch-hls <manifest> --peer … --out …`.
- **Reste** : feed records dans la DHT (PUT/GET), IPNS durable (différé).

**Phase 3 — en cours.** **Contrat UniFFI v3** : objet `ChampiniumNode`
(`open_node`, `listen`, `connect`, `catalog`, `ingest_file`, `publish_feed`,
`fetch_hls`, `subscribe_denylist`, `set_catalog_listener` — async sauf
`peer_id`/`catalog`), record `FfiCatalogEntry`, erreur **`FfiError` typée**
(`Moderated`/`Network`/`NotFound`/`InvalidInput`/`Internal` — les fronts
branchent une UX par catégorie), callback interface **`CatalogListener`**
(`on_catalog_updated`) : le noyau notifie chaque changement effectif du
catalogue, les fronts rafraîchissent réactivement (plus de délai gossip codé en
dur — le risque #1, async/callbacks via FFI, est éprouvé vers Swift ET C#).
Bindings Swift **et** C# générés et vérifiés pour toute la surface.
**UI macOS (SwiftUI) ✔ (compile)** : `apps/macos` consomme l'XCFramework + le
wrapper généré (`just macos-prepare`) ; `ContentView` fait openNode → listen →
connect → catalogue → `fetchHls` → lecture **AVPlayer**. `swift build` OK
(compile + link contre le binding réel). Lecture GUI à valider hors headless.
Voir [`AGENTS.md`](AGENTS.md) pour le tableau du contrat.

**Critère de sortie MVP (Phase 3) déroulé (historique)** — à l'origine prouvé
via seed-what-you-consume (démo du 2026-07-04, v0.2.0) : A ingère/publie, B
découvre par gossip et reconstruit un HLS jouable, puis — A éteint — un nœud C
obtient le contenu identique depuis B seul.
[`docs/mvp-demo.md`](docs/mvp-demo.md) a depuis été réécrit sur le flux
d'abonnement (channels lot (c), voir plus bas), qui a remplacé
seed-what-you-consume comme mécanisme de persistance. Reste à rejouer en GUI
sur deux machines physiques.

**Phase 4 — close.**
- **Front Linux GTK4 ✔ (feature `gui`)** : `apps/linux` consomme le crate ; UI =
  ouverture nœud → listen → connect → catalogue (refresh **réactif** via
  `subscribe_catalog`, en-tête « créateur X — seq N » par émetteur) → lecture
  **GStreamer** (`playbin`), pont tokio ↔ thread GTK. Gatée par `gui` pour garder
  le workspace vert sans GTK. Compilation vérifiée par le job CI `linux-gui`.
- **Relay NAT ✔** : circuit relay v2 + DCUtR côté client dans le noyau
  (`with_relay_client`), serveur de relais stateless `relay::start_relay` (qui
  déclare son adresse externe via `add_external_address` — sinon les réservations
  sont acceptées sans adresse). `infra/relay` = binaire réel. **Testé** : un nœud
  derrière « NAT » écoute via circuit, un autre l'atteint *via le relais* et
  récupère un bloc (`block_transfer_over_relay_circuit`).
- **Seeding en arrière-plan ✔** : `Node::reprovide_all` réannonce tous les CIDs
  détenus (le store de providers Kademlia est volatile → indispensable au
  redémarrage). Démon `champinium-seed` (réannonce + republication périodiques,
  hors UI) ; fichiers de service par OS dans `infra/services/` (launchd / systemd
  user / Windows). Testé : `reprovide_makes_stored_blocks_discoverable`.
- **Feed records DHT (PUT/GET) ✔** : `publish_feed` PUT le feed signé dans la
  Kademlia sous `/champinium/feed/<peerid>` ; `Node::fetch_feed` GET + vérifie
  (signature + émetteur) + alimente le catalogue → découverte de feed **hors
  gossip**. CLI : `fetch-feed --issuer <peerid>`. Testé :
  `feed_is_discoverable_via_dht_without_gossip`.
- **Robustesse fetch ✔** : `get()` interroge **tous les fournisseurs en
  parallèle** (première réponse valide gagne) et **réannonce** le bloc consommé
  (le consommateur devient fournisseur → réplication). Testé :
  `consumer_reseeds_to_other_peers`.
- **Front Windows ✔** : `apps/windows` WinUI 3 / C# (catalogue +
  MediaPlayerElement) contre les bindings UniFFI C#. Compilation vérifiée par le
  job CI `windows-app` (runner .NET Windows — non buildable en dev macOS).
- **libp2p 0.56 ✔** : socle réseau remonté à la version courante.
- **Peer scoring gossipsub ✔** : validation applicative des feeds
  (`validate_messages` — Accept/Ignore/Reject rapportés par la boucle
  d'évènements) + scoring : un pair qui inonde le topic de feeds invalides voit
  son score chuter, n'est plus relayé puis est graylisté. Complète la borne du
  catalogue (1024 émetteurs, refus-quand-plein) posée au durcissement.
  `Node::gossip_peer_score` pour l'observabilité. Testé (score négatif observé +
  régression : un feed valide traverse toujours un saut de relais gossip).
- **Durcissement post-audit ✔** : écriture atomique du blockstore (réparation
  des blocs corrompus + re-fetch réseau), catalogue borné anti-DoS, plafonds de
  tailles réseau, souscription de denylist à chaud avec purge rétroactive
  (`subscribe_denylist`), clé privée en 0600, `paths::default_data_dir()` durable
  par OS, signatures par champs préfixés par longueur (anti-malléabilité),
  nettoyage des répertoires de lecture temporaires dans les 3 fronts.
- **Déploiement tiers documenté ✔** : guide opérateur
  [`docs/deploy-bootstrap-relay.md`](docs/deploy-bootstrap-relay.md)
  (bootstrap 4101/tcp, relay 4201/tcp, systemd, périmètre : de la connectivité,
  pas de contenu).
- **bitswap — bloqué en amont (différé)** : l'implémentation maintenue (beetswap)
  cible libp2p 0.56 (d'où l'upgrade) mais sa dépendance transitive `core2` est
  **entièrement yankée et sans source git** → graphe non résoluble actuellement.
  Le **fetch concurrent multi-fournisseurs** (ci-dessus) couvre déjà le bénéfice
  pratique de bitswap ; à reprendre quand `core2`/`multihash-codetable` amont
  seront réparés (ou via vendoring si réellement nécessaire). Le transport reste
  `request-response` en attendant.

**Phase 5 — en cours.**
- **Signalement P2P ✔** : quand le checkpoint #2 refuse un CID, le nœud émet un
  rapport signé `champinium-report/v1` sur le topic `champinium/reports/v1`
  (best-effort). Les pairs vérifient et agrègent (borné : 10 000 CIDs, 1 000
  rapporteurs/CID) le nombre de rapporteurs **distincts** par CID — matière pour
  les éditeurs de denylists, **aucun effet automatique**. Topic couvert par la
  validation applicative + peer scoring. `Node::report_count(s)`.
- **Réplication mesurée ✔** : `Node::replication_factor(cid)` (fournisseurs
  DHT), CLI `replication <cid> --peer …`. Testé à l'époque (avant le retrait de
  seed-what-you-consume, lot (c) channels) : 1 → 2 après un `get` simple ; ce
  comportement a depuis été inversé par défaut, voir lot (c) ci-dessous.
- **Channels lot (a) ✔** : feed `champinium-feed/v3` (identité de channel
  signée : nom/description/avatar, formats v1/v2 supprimés — zéro
  utilisateur), profil persisté (`.channel_profile`) avec republication au
  changement, contrat FFI v5 (`FfiChannelProfile`,
  `set_channel_profile`/`channel_profile`, `FfiCatalogEntry.channel`). Spec :
  `~/Work/.superpowers/champinium/specs/2026-07-22-channels-subscriptions-design.md`.
- **Channels lot (b) ✔** : abonnements à un émetteur, **locaux et privés —
  jamais publiés** (store `.subscriptions` à côté des blocs, à côté de
  `.channel_profile` ; nuance : le suivi actif reste observable sur le réseau,
  voir `docs/architecture.md` §6). Suivi actif de chaque channel souscrit : fetch
  immédiat à l'abonnement, passe **périodique** en tâche de fond
  (`FOLLOW_INTERVAL`), rattrapage au **démarrage** du nœud (les abonnements
  persistés sont rechargés avant le premier tour de boucle). Un émetteur
  souscrit **franchit la borne du catalogue** (1024 émetteurs, sinon
  refus-quand-plein) — s'abonner garantit de voir le channel même si le
  catalogue est plein. Liens partageables `champinium://channel/<peerid>`
  (module `channel_link`, tolérant à un PeerId nu). Contrat FFI **v6**
  (`subscribe_channel`, `unsubscribe_channel`, `subscriptions`,
  `catalog_subscribed`, `channel_link`). CLI : `subscribe <lien-ou-peerid>` /
  `unsubscribe <peerid>` / `subscriptions` / `catalog --subscribed`. Les
  trois fronts ont une vue **Abonnements** (par défaut) et une vue
  **Explorer** (catalogue complet, opt-in derrière un avertissement) avec
  désabonnement possible depuis les deux. L'enregistrement OS du scheme
  `champinium://` (Info.plist / appxmanifest / .desktop) est différé au
  packaging (Phase 6) — coller le lien reste manuel jusque-là.
- **Channels lot (c) ✔** : politique de stockage explicite par appel,
  **`StorePolicy::Stream` par défaut** (`get` ne met plus le bloc en cache et
  n'annonce plus le consommateur comme fournisseur) — **seed-what-you-consume
  est retiré** (`StorePolicy::Seed` reste disponible en interne, utilisé par le
  seed proactif lui-même). Persistance reprise par une **boucle de seed
  proactif** : chaque nœud retient et resert les publications des channels
  **souscrits**, en **round-robin** sur ses abonnements, sous un **quota**
  d'octets persisté (`.seed_quota`, 20 Gio par défaut). Sous pression de
  quota, **éviction par réplication** (la publication déjà la mieux répliquée
  ailleurs sur le réseau part en premier, la plus ancienne en cas d'égalité) —
  amortie par une **inégalité stricte anti-oscillation** : on n'évince que si
  la victime potentielle est *strictement* mieux répliquée que le candidat
  entrant, jamais à égalité. **Pins** : un manifeste épinglé n'est jamais
  évincé ; tout contenu **publié par le nœud lui-même est auto-épinglé** à
  l'ingestion. **Désabonnement** : purge du `SeedIndex` les publications NON
  épinglées de l'émetteur retiré (les pins survivent au désabonnement) ; les
  blocs orphelins (non référencés par une autre publication indexée) sont
  supprimés. Index persisté `SeedIndex` (fichier dotfile `.seed_index`, à côté
  des blocs) — logique pure, séparée du réseau (`crates/champinium-core/src/
  seeding.rs`). **Réplication toutes-directions (opportuniste au-delà des
  abonnements) SUPPRIMÉE à dessein** — ne pas la réintroduire hors d'une
  décision explicite de spec ; `replicate_under_provided` et les flags du
  démon associés (`--replication-target`/`--replicate-max`) sont retirés.
  **Contrat FFI v7** : `seed_quota()`/`set_seed_quota(bytes)`,
  `storage_stats() -> FfiStorageStats`, `pin_content`/`unpin_content`,
  `FfiCatalogEntry` gagne `seeded_count`/`total_count`/`pinned`, callback
  **`SeedListener`** (`on_seed_updated`). CLI : `quota [--set <octets>]` /
  `pin <cid-manifeste>` / `unpin <cid-manifeste>`. Les trois fronts affichent
  l'état de seed par channel (« à jour » / « seed en cours (x/y) ») et des
  actions pin/unpin. Spec :
  `~/Work/.superpowers/champinium/specs/2026-07-22-channels-subscriptions-design.md`.
- **Channels lot (d) ✔** — clôt la refonte channels. **Modération par clé
  (denylist v2)** : `champinium-denylist/v2` gagne `key_entries` (PeerIds bannis
  en entier — tout contenu de la clé refusé, quel que soit le CID), v1 (CIDs
  seuls) **supprimé** (zéro-compat, comme le feed v3) ; les deux collections
  signées indépendamment (préfixe-longueur), borne cumulée 65 536. Enforcement
  par clé aux mêmes points : rejet du feed au catalogue, purge rétroactive à la
  souscription (toute entrée déjà présente d'un émetteur banni, pas seulement
  les clés de la liste souscrite), refus de `subscribe_channel` sur une clé
  bannie (`Moderated`). **Nuance anti-censure fondatrice** : bannir une clé
  bloque le contenu que **ce nœud a lui-même attribué** à l'émetteur — **aucune
  liste de CIDs dérivée des feeds** n'est construite, car lister un CID ne
  prouve pas sa propriété ; dériver un blocklist des feeds laisserait un émetteur
  censurer le contenu d'un tiers en le listant avant de se faire bannir (censure
  par injection). Invariant à préserver. **Blocage local de channel**
  (`block_channel`/`unblock_channel`/`blocked_channels`, dotfile privé
  `.blocked_channels`) : préférence **strictement locale et privée, aucun effet
  réseau** (pas de rapport, pas de record) ; le channel disparaît des deux vues,
  désabonnement inclus, et la **purge locale outrepasse les pins** (l'utilisateur
  qui bloque efface tout, pins compris — contrairement à l'éviction de quota).
  **Purge rétroactive étendue** (`purge_blocked_issuer`, partagée ban-clé +
  blocage local) : catalogue + SeedIndex + blocs orphelins **+ `stop_providing`**
  (retrait immédiat du provider record local — `libp2p-kad` 0.48 l'expose, pas
  de repli TTL nécessaire ; les copies distantes déjà propagées expirent par leur
  propre TTL). L'éviction de quota (lot c) n'appelle **pas** `stop_providing`
  (stabilité de l'amortisseur) — la modération, si. **Signalements par channel**
  (`report_counts_by_channel`, lecture seule) : jointure locale rapports×catalogue
  → `(rapporteurs distincts cumulés, CIDs distincts signalés)` par émetteur, aide
  les éditeurs de denylists à repérer un candidat `key_entries`, **aucun effet
  automatique**. `fetch_hls(Seed)` entre désormais au SeedIndex. **Contrat FFI
  v8** (`block_channel`/`unblock_channel`/`blocked_channels`). CLI : `block
  <lien-ou-peerid>` / `unblock <peerid>` / `blocked` / `reports --by-channel`.
  Les trois fronts ont l'action « Bloquer » (vue Explorer) + un volet « Channels
  bloqués ». Spec :
  `~/Work/.superpowers/champinium/specs/2026-07-22-channels-subscriptions-design.md`.
- **Aperçu de channel par lien ✔** : `Node::resolve_channel(peer_id)` résout un
  aperçu **catalogue d'abord** (zéro appel réseau si l'émetteur y figure déjà),
  sinon DHT (`fetch_verified_feed_from_dht`) puis relecture ; introuvable →
  `NotFound`. **Décision produit** : coller un lien `champinium://channel/…`
  (ou un PeerId nu) **ne s'abonne plus directement** — il ouvre d'abord une
  fiche d'aperçu (nom/description/avatar, contenus connus,
  `subscribed`/`blocked`), et c'est depuis cette fiche que l'utilisateur choisit
  explicitement de s'abonner ; l'abonnement direct au collage est retiré des
  trois fronts. **Nuance modération** : un émetteur **bloqué localement reste
  résolvable** (`blocked = true`, aperçu autorisé pour pouvoir décider un
  déblocage en connaissance de cause) mais son feed **n'entre jamais au
  catalogue** — le chemin bloqué construit l'aperçu directement depuis le feed
  vérifié en DHT, sans passer par `Catalog::apply` (le checkpoint de
  modération à l'ingestion continue de rejeter ces feeds à l'entrée du
  catalogue, lot d ; `resolve_channel` ne contourne pas ce rejet, il construit
  juste un instantané en plus). Contrat FFI **v9** : `resolve_channel(lien-ou-
  peerid) -> FfiChannelPreview` (async), record `FfiChannelPreview { peer_id,
  name, description, avatar_cid, items, subscribed, blocked }` ;
  `subscribe_channel`/`unsubscribe_channel` inchangés (toujours contrat v6).
  Les trois fronts : champ de collage de lien → bouton « Aperçu » (état de
  chargement pendant l'appel async) → fiche/feuille/fenêtre d'aperçu avec
  « S'abonner » / « Se désabonner » selon `subscribed`, ou « Channel bloqué »
  si `blocked`. **Partie B** (enregistrement du scheme `champinium://` auprès
  de l'OS — `CFBundleURLTypes`/`onOpenURL`, `x-scheme-handler` + instance
  unique, registre Windows — pour ouvrir un lien d'un clic depuis un
  navigateur) reste **différée à la Phase 6 (packaging)**, sur ce même
  `resolve_channel` : coller le lien manuellement dans l'app reste le seul
  chemin jusque-là.
- **Durabilité du record de feed ✔** : `Node::republish_known_feeds`
  (`champinium-seed`, même boucle que `reprovide_all`) re-PUT dans la DHT le
  feed signé du nœud lui-même et ceux de ses **abonnements** — corrige un
  écart où le record `/champinium/feed/<peerid>` d'un créateur hors ligne
  n'était jamais réannoncé (contrairement à ce que l'ADR 0007 supposait déjà
  couvert), le laissant s'éteindre au TTL du `MemoryStore` Kademlia.
- IPNS durable (#21) **fermée par l'ADR 0007** (différée : la durabilité
  pratique est déjà assurée par le point ci-dessus, l'interop IPFS public
  reste bloquée par ailleurs — voir [`docs/adr/0007-ipns-deferred.md`](docs/adr/0007-ipns-deferred.md)).
  **La refonte channels (lots a–d) est intégralement livrée.**

**Stockage froid CS-a ✔ (ADR 0008)** — filet de dernier recours contre la perte
d'un contenu sans abonné, entièrement **derrière une feature cargo opt-in
`cold-storage`** (absente des builds par défaut : ni `rsa`, ni `reqwest` tirés,
`cargo deny` reste sans ignore CVE puisque la surface crypto n'entre pas dans le
graphe par défaut). Livré :
- **Trait `ColdStore`** (`retrieve`/`archive`/`price`/`balance`), backend Arweave
  `ArweaveColdStore` (module `coldstore/`). Signature de transaction **hand-roll**
  (aucune lib Arweave) : **deep-hash** (SHA-384 en accumulateur, format 2) pour
  les octets à signer, **RSA-PSS** (SHA-256, sel 32, `BlindedSigningKey`
  aveuglé — mitigation Marvin) via `rsa` **0.10-rc** (pré-release, feature-gated
  donc contenue ; à repasser stable quand publiée), tx-id = `SHA-256(signature)`.
  Portefeuille JWK apporté par le créateur (chemin, permissions 0600).
- **Repli de récupération froide dans `Node::get_with`** : uniquement sur
  `NoProviders` (jamais chemin principal), **CID-vérifié** avant tout usage (les
  gateways peuvent servir du silence, jamais du faux), puis flux normal —
  politique Seed/Stream inchangée (souscrit → **réamorce le P2P**), checkpoint de
  modération #2 inchangé. **Débrayable** (dotfile `.cold_enabled`, actif par
  défaut) : interroger une gateway par CID révèle l'intérêt de l'IP, surface
  d'observation documentée avec la même franchise que le suivi actif.
- **Archivage en deux temps, créateur-paie** : `archive_publication` (devis :
  taille, coût AR estimé, solde) puis `confirm_archive` (envoi) — jamais
  silencieusement payant. Reçus locaux persistés (`.archives`, purement
  informatifs). **Forme par item-tx : une transaction Arweave par item
  (manifeste + chaque segment), imposée par la récupération par CID** (chaque CID
  doit être adressable/vérifiable seul) — **s'écarte du §Archivage de la spec
  (« une transaction/bundle ») à dessein**, décision correcte.
- **CLI** (gatée par la feature `cold-storage` du CLI) : `archive <cid-manifeste>`
  (devis + confirmation), `archives` (liste des reçus), `cold-retrieval [--set …]`
  (réglage du repli).
- **Test d'intégration Arweave réel** : `#[ignore]` **et** gaté par
  `CHAMPINIUM_ARWEAVE_IT=1` (+ `CHAMPINIUM_ARWEAVE_JWK`) — coûte de vrais AR, un
  `cargo test --features cold-storage` normal ne le lance jamais ; le job CI
  `cold-storage` build+clippy+teste la feature **sans** aucune variable réseau.
- **CS-b hors périmètre** (fronts ×3 : bouton « Archiver » + devis, liste
  « mes archives », réglage du repli ; contrat FFI v10 ; Filecoin via le trait).

Phasing : 0 (spike async FFI ✔ contrat) → **1 (P2P nu CLI ✔)** → **2 (modération ✔,
feeds/gossipsub/catalogue ✔, ingestion ffmpeg ✔)** → **3 (contrat UniFFI v3 ✔,
UI macOS compile ✔, critère MVP déroulé ✔)** → **4 (close : 3 fronts ✔, relay
NAT ✔, seeding ✔, feed DHT ✔, fetch concurrent ✔, déploiement tiers documenté ✔ ;
bitswap différé)** → 5 (en cours : peer scoring ✔, signalement P2P ✔, réplication
mesurée ✔, recherche ✔ (#20) ; **refonte channels COMPLÈTE** — lot (a) identité
✔, lot (b) abonnements ✔, lot (c) seed proactif/quota/pins ✔, lot (d) modération
par clé + blocage local + signalements par channel ✔ ; aperçu de channel par
lien ✔ (`resolve_channel`, contrat v9, partie B différée Phase 6) ; durabilité
du record de feed ✔ (`republish_known_feeds`) ; IPNS #21 close, voir ADR 0007).
Voir le spec.

**Dernière release : voir `.release-please-manifest.json` / CHANGELOG** —
pas de version en dur ici, elle dérive (règle intendant DG006). Release-please
gère le versionnement (`bump-minor-pre-major` actif :
un breaking change bumpe la mineure tant qu'on est < 1.0.0 — la 1.0 sera un
choix délibéré de stabilisation d'API). Versionnement du contrat FFI distinct :
`CONTRACT_VERSION = 9` (voir `AGENTS.md`).
