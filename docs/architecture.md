# Architecture de Champinium — le guide complet

> Public : quiconque veut comprendre comment le projet fonctionne de bout en
> bout. Les renvois pointent vers le code (chemins cliquables) et les ADRs
> (`docs/adr/`) pour les décisions. État au contrat FFI **v8** (voir
> `.release-please-manifest.json` / `CHANGELOG.md` pour la version de release
> — elle dérive, pas de version en dur ici, cf. `CLAUDE.md`).

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
| [`catalog`](../crates/champinium-core/src/catalog.rs) | CRDT maison : map last-writer-wins par émetteur, bornée (1024 émetteurs, sauf émetteurs souscrits — §6) ; recherche locale |
| [`moderation`](../crates/champinium-core/src/moderation.rs) | denylist compilée (non désactivable) + denylists signées souscrites (fédéré), **v2** = CIDs **et** clés (`key_entries`, ban d'émetteur entier) ; blocage local privé de channel côté `Node` (§7) |
| [`report`](../crates/champinium-core/src/report.rs) | signalement P2P : rapport signé + agrégateur borné de rapporteurs distincts |
| [`ingest`](../crates/champinium-core/src/ingest.rs) | orchestration ffmpeg → segments HLS alignés keyframes → manifeste `champinium-hls/v1` |
| [`p2p`](../crates/champinium-core/src/p2p.rs) | le cœur : `Node`, la boucle d'évènements libp2p, tous les flux (§4, §6), le suivi actif des abonnements |
| [`channel_link`](../crates/champinium-core/src/channel_link.rs) | lien partageable `champinium://channel/<peerid>` d'un channel (format/parse, tolérant à un PeerId nu) |
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
- **Denylist** (`champinium-denylist/v2`, JSON signé) : liste signée par son
  éditeur, portant des **CIDs** bloqués (`entries`) **et** des **clés**
  bloquées (`key_entries` — PeerIds d'émetteurs bannis en entier ; tout contenu
  de cette clé est refusé quel que soit son CID). Les deux collections sont
  couvertes indépendamment par la signature préfixée-longueur. Le format **v1
  (CIDs seuls) a été supprimé** (zéro-compat, comme le feed v3). Voir
  [`deny/README.md`](../deny/README.md) et §7.
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

### Abonnements : suivi actif d'un émetteur choisi

Au-delà des trois chemins de découverte passive ci-dessus, un nœud peut
**s'abonner** à un émetteur (`subscribe_channel(lien-ou-peerid)`) — la liste
d'abonnements est **locale, privée et jamais publiée** (fichier
`.subscriptions` à côté des blocs, comme `.channel_profile`). L'abonnement :

- persiste immédiatement, puis déclenche un **fetch immédiat** en tâche de
  fond ;
- est ensuite suivi **activement** par une boucle périodique
  (`FOLLOW_INTERVAL`) qui refait `fetch_feed` pour chaque émetteur souscrit,
  sans dépendre du gossip ni d'une action de l'utilisateur ;
- est **rechargé au démarrage** du nœud, avant le premier tour de boucle, de
  sorte qu'un redémarrage rattrape les feeds manqués pendant l'arrêt.

Un émetteur souscrit **franchit la borne anti-DoS du catalogue** (1024
émetteurs, §7) : même si le catalogue est plein, ses feeds sont toujours
appliqués — s'abonner garantit de voir ce channel. Les liens partageables
`champinium://channel/<peerid>` (module `channel_link`) sont la forme
échangée entre utilisateurs (« copier le lien de mon channel ») ; `parse` est
tolérant et accepte aussi un `PeerId` nu. Se désabonner (`unsubscribe_channel`)
retire le suivi et la vue, **et** purge le stock proactif de seed constitué
pour cet émetteur (§6 bis) — sauf les manifestes épinglés, qui survivent au
désabonnement.

Nuance à connaître : la **liste** d'abonnements elle-même n'est jamais
publiée, mais le suivi actif émet un `fetch_feed` — donc un GET DHT — vers
`/champinium/feed/<peerid>` à chaque passe périodique. L'intérêt d'un nœud
pour un channel donné est donc **observable** par les pairs Kademlia situés
sur le chemin de cette requête. C'est inhérent au suivi actif voulu par la
spec (§2), pas un défaut d'implémentation — mais ça mérite d'être dit
explicitement plutôt que de laisser croire à une confidentialité totale.

### Aperçu de channel par lien : prévisualiser avant de s'abonner

Coller un lien `champinium://channel/<peerid>` (ou un PeerId nu) dans un des
trois fronts **ne s'abonne plus directement** — c'est une décision produit
délibérée : le collage ouvre d'abord une **fiche d'aperçu**
(`resolve_channel`), et c'est depuis cette fiche que l'utilisateur choisit
explicitement de s'abonner. Le champ de collage → bouton « Aperçu » (état de
chargement pendant l'appel réseau, potentiellement lent) → fiche affichant
nom/description/avatar + contenus connus, avec « S'abonner » / « Se
désabonner » selon l'état réel d'abonnement, ou « Channel bloqué » si
l'émetteur est bloqué localement (§7).

`Node::resolve_channel` résout **catalogue d'abord** (zéro appel réseau si
l'émetteur y figure déjà), sinon via la DHT (même vérification
signature/émetteur que `fetch_feed`), puis relit le catalogue qu'il vient
d'alimenter ; introuvable des deux côtés → `NotFound`.

Nuance de modération à connaître : un émetteur **bloqué localement reste
résolvable** — l'aperçu reste consultable (`blocked = true`) pour que
l'utilisateur puisse revoir ce qu'il a bloqué avant, par exemple, un
déblocage. Mais le checkpoint de modération à l'ingestion (lot d) continue de
**rejeter les feeds d'une clé bloquée avant `Catalog::apply`** : le feed d'un
émetteur bloqué **n'entre donc jamais au catalogue**, quel que soit le nombre
d'aperçus consultés. Pour honorer les deux à la fois, le chemin bloqué de
`resolve_channel` interroge la DHT lui-même et construit l'aperçu directement
depuis le `Feed` vérifié, sans jamais passer par `Catalog::apply` — l'aperçu
d'un channel bloqué est un instantané pur, il ne contourne pas le rejet à
l'ingestion.

**Partie B** (enregistrement du scheme `champinium://` auprès de l'OS —
`CFBundleURLTypes`/`onOpenURL` macOS, `x-scheme-handler` + instance unique
Linux, registre Windows — pour ouvrir un lien d'un clic depuis un navigateur,
plutôt que de le coller manuellement) est **différée à la Phase 6
(packaging)**, sur ce même `resolve_channel`.

### Lecture (`StorePolicy::Stream` par défaut)

```
clic « Lire » → fetch_hls(manifeste)
  get(manifeste) : cache local ? sinon [modération #2] → providers DHT
      → requêtes à TOUS les fournisseurs EN PARALLÈLE, 1ʳᵉ réponse valide gagne
      → bloc vérifié contre le CID → rendu à l'appelant, PAS mis en cache,
        PAS d'annonce fournisseur (StorePolicy::Stream)
  idem pour chaque segment → écrit index.m3u8 + .ts → lecteur natif
  (échec en route → répertoire de sortie nettoyé, pas de lecture partielle)
```

`get` prend une politique de stockage explicite (`crate::p2p::StorePolicy`,
interne) : **`Stream`** (défaut de toute lecture/consommation — rend les
octets, ne cache pas, n'annonce pas) ou **`Seed`** (met en cache et s'annonce
fournisseur). **`seed-what-you-consume` est retiré** : un simple visionnage ne
fait plus du spectateur un fournisseur — testé par
`consumer_does_not_reseed_by_default`
([`crates/champinium-core/tests/replication.rs`](../crates/champinium-core/tests/replication.rs)).
`StorePolicy::Seed` reste utilisé en interne par la boucle de seed proactif
ci-dessous (§6 bis) — c'est elle, pas la lecture, qui décide quoi retenir.

### Seed proactif des abonnements : quota, éviction, pins (§6 bis)

Depuis le retrait de seed-what-you-consume, la persistance du contenu dépend
d'un mécanisme **explicite et borné** plutôt que d'un effet de bord de la
lecture : chaque nœud retient et resert proactivement les publications des
channels **auxquels il est abonné**.

- **`SeedIndex`** ([`crates/champinium-core/src/seeding.rs`](../crates/champinium-core/src/seeding.rs)) :
  logique pure (aucun accès réseau), persistée en JSON dans le dotfile
  **`.seed_index`** à côté des blocs (comme `.channel_profile`,
  `.subscriptions`). Indexe les publications retenues par émetteur, un
  ensemble de manifestes **épinglés** (`pin`/`unpin`, exemptés d'éviction) et
  un compteur d'octets total.
- **Quota** : un budget d'octets, persisté dans le dotfile **`.seed_quota`**
  (20 Gio par défaut, `DEFAULT_SEED_QUOTA_BYTES`), modifiable via
  `set_seed_quota(bytes)`.
- **Boucle de seed** (`seed_loop`/`seed_pass` dans
  [`p2p.rs`](../crates/champinium-core/src/p2p.rs)) : passe périodique en
  **round-robin** sur les émetteurs souscrits (l'index de départ tourne à
  chaque passe, pour ne pas toujours favoriser le même émetteur en tête de
  liste) ; réveillée aussi par changement de catalogue, de quota ou
  désabonnement (`subscribe_seed`/canal de réveil dédié, pas seulement le
  minuteur).
- **Éviction sous pression de quota** (`make_room_for`/`eviction_order`) :
  quand une nouvelle publication ne rentre plus sous quota, la victime est
  choisie par **réplication décroissante** (ce qui est déjà bien répliqué
  ailleurs sur le réseau part en premier — mesurée via les providers DHT du
  manifeste, `get_providers`), puis par ancienneté croissante en cas
  d'égalité. Les manifestes **épinglés** sont exclus des candidats à
  l'éviction.
- **Amortisseur anti-oscillation (dampener)** : l'éviction n'a lieu que si la
  réplication de la victime potentielle est **strictement supérieure** à
  celle du candidat entrant (`victim_replication > candidate_replication`,
  jamais `>=`) — sans cette inégalité stricte, deux nœuds à la limite de leur
  quota et à réplication égale s'évinceraient mutuellement en boucle
  (thrashing) au lieu de stabiliser leur seed. C'est un compromis assumé :
  sous cette condition, une publication un peu trop volumineuse peut rester
  bloquée hors quota plutôt que de forcer une éviction à réplication égale.
- **Pins** : `pin_content(manifest_cid)`/`unpin_content(manifest_cid)`
  épinglent/dé-épinglent un manifeste manuellement. **Tout contenu publié par
  le nœud lui-même est auto-épinglé à l'ingestion** (`ingest_file`) — un
  créateur ne voit jamais son propre contenu évincé par sa propre boucle de
  seed.
- **Désabonnement** (`unsubscribe_channel`) : purge du `SeedIndex` les
  publications **non épinglées** de l'émetteur retiré ; les blocs devenus
  orphelins (non référencés par une autre publication encore indexée) sont
  supprimés du blockstore. Les manifestes épinglés de cet émetteur survivent
  au désabonnement.
- **Couplage avec les provider records DHT** : l'éviction retire la
  publication du `SeedIndex` et supprime ses blocs devenus orphelins, mais ne
  retire **pas** le provider record Kademlia annoncé pour ce CID (pas
  d'appel `stop_providing`/`unprovide`) — celui-ci s'éteint naturellement par
  expiration TTL côté DHT. La stabilité de l'amortisseur anti-oscillation
  ci-dessus **dépend de ce choix** : mesurer la réplication en incluant un
  provider record qu'on vient soi-même de retirer changerait le calcul
  d'éligibilité à la volée et réintroduirait le risque de thrashing que le
  dampener cherche à éviter. (Ce choix est **propre à l'éviction de quota** : la
  purge de **modération**, elle, appelle bien `stop_providing` pour cesser
  d'annoncer un contenu banni — voir §7.)
- **Réplication toutes-directions supprimée à dessein** : le lot (c) retire
  aussi `replicate_under_provided` et les flags de démon associés
  (`--replication-target`/`--replicate-max`) — un nœud ne réplique plus
  spontanément du contenu hors de ses abonnements. Ne pas réintroduire cette
  réplication opportuniste sans décision explicite de spec (règle §3 de la
  spec channels).

Consommer ne réplique plus automatiquement (§ ci-dessus) : la mitigation du
risque « un contenu sans seeder disparaît » (risque #1) repose désormais sur
le seed proactif des abonnés et les pins, pas sur la lecture.

### Persistance active

- **`champinium-seed`** (démon, fichiers de service dans
  [`infra/services/`](../infra/services)) : depuis le retrait de
  seed-what-you-consume, le démon **resert seulement ce qu'il détient déjà** —
  il réannonce périodiquement tous les CIDs détenus (le store de providers
  Kademlia est volatil) via `reprovide_all`. Il **ne publie plus de feed**
  (la publication reste le rôle du nœud créateur, pas du démon) et **ne fait
  plus de réplication opportuniste** au-delà des abonnements (voir §6 bis,
  ci-dessus) : ce qu'il détient à seeder est entièrement décidé par la boucle
  de seed proactif du nœud, sur ses propres abonnements.
- Un bloc local **corrompu** (crash pendant écriture) n'est pas fatal : `get`
  détecte l'incohérence d'intégrité et re-télécharge du réseau, ce qui répare
  le fichier.

### Stockage froid : filet de dernier recours opt-in (ADR 0008, lot CS-a)

Depuis le retrait de seed-what-you-consume, un contenu sans abonné disparaît
quand son créateur s'éteint (choix assumé). Le **stockage froid** offre à un
créateur qui tient à la pérennité de son contenu un filet **optionnel**, sans
imposer de coût à personne ni composant central. Tout vit **derrière la feature
cargo `cold-storage`**, absente des builds par défaut : aucune dépendance
supplémentaire (`rsa`, `reqwest`) n'entre dans le graphe par défaut — la surface
crypto reste hors de portée de `cargo deny` tant que la feature n'est pas
activée, d'où l'absence d'ignore CVE.

- **Trait `ColdStore`** (`retrieve`/`archive`/`price`/`balance`) — isole le
  backend ; Filecoin pourra s'ajouter sans toucher au repli ni à l'archivage.
  Seul backend aujourd'hui : `ArweaveColdStore` (module
  [`coldstore/`](../crates/champinium-core/src/coldstore)).
- **Signature de transaction hand-roll** (aucune lib Arweave, décision
  supply-chain) : **deep-hash** (accumulateur SHA-384, transaction format 2)
  produit les octets à signer ; signature **RSA-PSS** (SHA-256, longueur de sel
  32, `BlindedSigningKey` *aveuglé* — mitigation Marvin) via `rsa` **0.10-rc**
  (pré-release, contenue par le feature-gating, à repasser stable une fois
  publiée) ; l'id de transaction est `SHA-256(signature)`. Le portefeuille JWK
  est apporté par le créateur (chemin, permissions 0600) — champinium ne crée,
  ne gère ni ne finance rien.
- **Récupération = repli, jamais chemin principal.** Le seul point d'entrée est
  `Node::get_with`, **uniquement quand le P2P conclut à `NoProviders`**. Tout
  octet récupéré est **vérifié contre son CID** avant tout usage (une gateway
  peut servir du silence, jamais du faux). Ensuite le flux normal reprend : la
  politique `Seed`/`Stream` est inchangée (souscrit → le contenu ré-entre au
  SeedIndex et **réamorce le réseau P2P** ; sinon lecture sans trace), et le
  **checkpoint de modération #2 est inchangé** — l'archive ne contourne aucune
  modération. Le repli est **débrayable** (dotfile `.cold_enabled`, actif par
  défaut) : interroger une gateway par CID révèle à cette gateway l'intérêt de
  l'IP pour ce CID — surface d'observation différente du P2P pur, documentée avec
  la même franchise que l'observabilité DHT du suivi actif (§6).
- **Archivage en deux temps, payé par le créateur.** `archive_publication`
  calcule un **devis** (taille, coût estimé en AR, solde du portefeuille) ;
  `confirm_archive` seul déclenche l'envoi — jamais automatique, jamais
  silencieusement payant. Reçus locaux (`.archives`, purement informatifs).
  **Forme par item-tx** : **une transaction Arweave par item** (manifeste HLS +
  chaque segment), imposée par la récupération par CID (chaque CID doit être
  adressable et vérifiable seul). Cela **s'écarte du §Archivage de la spec (« une
  transaction/bundle ») à dessein** — le bundle empêcherait la vérification
  d'un CID isolé au repli.
- **Test d'intégration Arweave réel** : `#[ignore]` **et** gaté par
  `CHAMPINIUM_ARWEAVE_IT=1` (+ `CHAMPINIUM_ARWEAVE_JWK`, JWK financé 0600) — il
  coûte de vrais AR ; un `cargo test --features cold-storage` normal ne le lance
  jamais. Le reste de la couverture (repli, deep-hash, RSA-PSS, devis) tourne
  contre des mocks, sans réseau. Le job CI `cold-storage` build+clippy+teste la
  feature **sans** aucune variable réseau (§10).
- **CLI** (gatée par la feature `cold-storage` du CLI) : `archive`, `archives`,
  `cold-retrieval`. Les fronts ×3 (bouton « Archiver », liste « mes archives »,
  réglage du repli, contrat FFI v10) sont **hors périmètre CS-a** (lot CS-b).

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

### Modération par clé (denylist v2)

Une denylist v2 ([`moderation`](../crates/champinium-core/src/moderation.rs))
peut bannir des **émetteurs entiers** (`key_entries`, PeerIds) en plus de CIDs
isolés (`entries`). Une clé bannie voit **tout** son contenu refusé, quel que
soit son CID, aux mêmes checkpoints — c'est la réponse au créateur récidiviste
qui republie sous de nouveaux CIDs. Bannir une clé est un acte **fédéré et
assumé** : un éditeur de denylist désigne nommément une identité (vérifiée par
sa clé) et signe ce choix ; ses abonnés en héritent explicitement.

**Absence délibérée d'un ensemble de CIDs bloqués dérivé des feeds.** Bannir
une clé bloque le contenu **attribué localement** à cette clé (via le catalogue
reconstruit et l'index de seed), pas une liste de CIDs *extraite des feeds* de
cette clé. La distinction est le cœur du design anti-censure : un feed ne
**prouve pas la propriété** d'un CID — n'importe qui peut lister le CID d'un
tiers dans son propre feed. Si bannir une clé purgeait tous les CIDs qu'elle
*mentionne*, un émetteur banni (ou un attaquant qui se fait bannir exprès)
pourrait faire **censurer le contenu légitime d'un tiers** en le listant dans
son feed avant le ban — une censure par injection. L'enforcement par clé se
limite donc strictement à ce que **ce nœud** a lui-même attribué à l'émetteur ;
aucun blocage n'est propagé vers des CIDs sur la seule foi d'un feed. C'est un
invariant à préserver : ne jamais dériver un blocklist de CIDs à partir du
contenu des feeds.

Enforcement d'une clé bannie, aux mêmes points que par CID :
- **catalogue** : un feed d'un émetteur banni est rejeté à l'ingestion
  (`fetch_feed_inner`/`handle_feed_message`), jamais appliqué ;
- **purge rétroactive à la souscription** : souscrire une denylist v2 purge
  immédiatement toute entrée de catalogue **déjà présente** dont l'émetteur est
  désormais banni (pas seulement les clés de *cette* liste — une clé a pu être
  bannie avant que son feed n'arrive) ;
- **abonnement refusé** : `subscribe_channel` sur une clé bannie échoue en
  `Moderated` (refus délibéré, pas erreur de saisie) — sinon un abonné
  exempterait l'émetteur de la borne du catalogue tout en le rejetant par
  ailleurs, et ferait tourner le suivi actif en pure perte.

### Blocage local de channel (strictement privé)

Distinct de la denylist fédérée : un utilisateur peut **bloquer localement** un
channel (`block_channel`, dotfile privé `.blocked_channels` à côté des blocs).
C'est une préférence **strictement locale et privée** — **aucun effet réseau** :
pas de rapport, pas de record signé, rien de publié (à l'inverse d'un ban de
denylist, qui est un choix fédéré partagé). Le channel disparaît des deux vues
(Abonnements et Explorer), n'est plus jamais retéléchargé, et l'abonnement
éventuel est retiré. La **purge locale outrepasse les pins** : contrairement à
l'éviction de quota (lot c), la modération locale supprime aussi les manifestes
épinglés de cet émetteur — un utilisateur qui bloque veut tout effacer, pins
compris. `unblock_channel` retire seulement la préférence ; rien n'est
retéléchargé automatiquement, le contenu revient naturellement au prochain feed
reçu de cet émetteur. `blocked_channels()` liste les PeerIds bloqués (le nom
n'est plus au catalogue après purge, d'où l'affichage par PeerId dans les
fronts).

### Purge rétroactive étendue (SeedIndex + `stop_providing`)

La purge de modération par émetteur (`purge_blocked_issuer`, partagée entre le
ban de denylist par clé et le blocage local) va au-delà du catalogue :

- retire l'entrée du **catalogue** local ;
- purge du **`SeedIndex`** les publications attribuées à l'émetteur (blocage
  local : **pins compris** ; ban denylist : idem via le même chemin) et
  **supprime les blocs devenus orphelins** du blockstore (un bloc encore
  référencé par la publication d'un autre émetteur non bloqué est préservé) ;
- **arrête d'annoncer ce nœud comme fournisseur** de chaque CID purgé
  (`stop_providing`). `libp2p-kad` 0.48 (via libp2p 0.56) expose
  `Behaviour::stop_providing`, qui retire **immédiatement le provider record
  local** — pas de repli sur la seule expiration TTL. Limite assumée : les
  copies du provider record déjà propagées chez des pairs distants ne sont pas
  rappelées et s'éteignent à leur propre TTL (best-effort, comme `provide`).
  `stop_providing` n'émet aucun rapport ni signal aux pairs — cohérent avec le
  caractère strictement local du blocage.

> Nuance vs l'éviction de quota (§6 bis) : l'éviction, elle, **n'appelle pas**
> `stop_providing` (elle retire du SeedIndex et supprime les orphelins, mais
> laisse le provider record expirer par TTL) — c'est voulu, la stabilité de
> l'amortisseur anti-oscillation en dépend. La purge de **modération** est le
> seul chemin qui retire activement le provider record local, parce qu'un
> contenu banni ne doit pas continuer à être annoncé.

Autour, trois mécanismes d'écosystème :

- **Signalement par CID** : les pairs agrègent les rapports par CID (rapporteurs
  **distincts**, agrégat borné) — matière première pour les éditeurs de
  denylists, **aucun effet automatique** sur le contenu.
- **Signalement par channel** (`report_counts_by_channel`, lot d) : jointure
  **locale, lecture seule** entre l'agrégat de rapports par CID et le mapping
  CID→émetteur du catalogue reconstruit — pour chaque émetteur, `(rapporteurs
  distincts cumulés, nombre de CIDs distincts signalés qui lui sont attribués)`.
  Aide un éditeur de denylist à repérer un émetteur globalement problématique
  (candidat à un `key_entries`). **Aucun effet automatique**, et limite assumée :
  un CID signalé absent du catalogue local (émetteur jamais vu) n'est pas
  attribué et reste compté au seul agrégat global. CLI : `reports --by-channel`.
- **Peer scoring gossipsub** : émettre des feeds/rapports invalides dégrade le
  score du pair → ses messages ne sont plus relayés → graylist. Avec le
  catalogue borné à 1024 émetteurs (refus-quand-plein, pas d'éviction), c'est
  la défense contre l'inondation par clés jetables.

## 8. La frontière FFI : le contrat v9

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
- **Aperçu de channel par lien (v9)** : `resolve_channel(lien-ou-peerid) ->
  FfiChannelPreview` (async), même tolérance de parsing que
  `subscribe_channel`/`block_channel` (lien `champinium://channel/<peerid>` ou
  PeerId nu). Record `FfiChannelPreview { peer_id, name, description,
  avatar_cid, items, subscribed, blocked }` — identité éditoriale aplatie,
  même choix que `FfiCatalogEntry`. `subscribe_channel`/`unsubscribe_channel`
  ne changent pas (toujours contrat v6) : l'aperçu et l'abonnement restent
  deux actions FFI distinctes, voir §6.
- **Erreurs typées** : `FfiError::{Moderated, Network, NotFound, InvalidInput,
  Internal}` — un contenu bloqué par la modération s'affiche « contenu
  bloqué », pas comme une panne réseau.
- **Bindings générés au build, jamais commités** : Swift via
  UniFFI/XCFramework (`just macos-prepare`), C# via `uniffi-bindgen-cs`
  (`just gen-csharp`). Le front Linux consomme le crate **directement** (pas
  de FFI). `CONTRACT_VERSION` (=9) permet aux fronts de détecter une
  incompatibilité au démarrage.
- **Abonnements (v6)** : `subscribe_channel`/`unsubscribe_channel` (lien
  `champinium://channel/<peerid>` ou PeerId nu), `subscriptions` (liste
  locale), `catalog_subscribed` (catalogue restreint aux émetteurs souscrits)
  et `channel_link` (lien partageable du channel de ce nœud). Aucune de ces
  méthodes ne touche au réseau de façon synchrone-bloquante côté front : le
  suivi actif tourne dans la boucle de fond du noyau (§6).
- **Seed proactif — quota, stats, pins (v7)** : `seed_quota()`/
  `set_seed_quota(bytes)`, `storage_stats() -> FfiStorageStats {used_bytes,
  quota_bytes}`, `pin_content(manifest_cid)`/`unpin_content(manifest_cid)`
  (CID invalide → `InvalidInput`). `FfiCatalogEntry` gagne `seeded_count`,
  `total_count` (couverture du seed proactif sur le feed courant de
  l'émetteur) et `pinned` (manifestes épinglés de ce feed) — rupture pour qui
  construit le record, d'où le bump de contrat. Callback interface
  **`SeedListener`** (`on_seed_updated()`), même patron que
  `CatalogListener` : notifie toute publication nouvellement seedée, éviction
  ou purge au désabonnement ; le front re-dispatche puis relit
  `storage_stats()`/`catalog()`.
- **Blocage local de channel (v8)** : `block_channel(lien-ou-peerid)` (async :
  persiste puis purge catalogue + SeedIndex + blocs + `stop_providing`, §7),
  `unblock_channel(peer_id)`, `blocked_channels() -> Vec<String>` (liste locale
  des PeerIds bloqués). Un lien `champinium://channel/…` ou un PeerId nu sont
  acceptés ; une entrée illisible → `InvalidInput`, une clé déjà bannie par
  denylist ou une opération de modération → `Moderated`. Le blocage est
  **strictement local** : aucune de ces méthodes n'émet quoi que ce soit sur le
  réseau. Côté fronts, l'action « Bloquer » vit dans la vue **Explorer** (on ne
  bloque pas un abonné sans se désabonner) et les channels bloqués se gèrent
  depuis un volet dédié **« Channels bloqués »** (déblocage par PeerId).

Les trois fronts ([`apps/macos`](../apps/macos),
[`apps/windows`](../apps/windows), [`apps/linux`](../apps/linux)) font tous la
même chose et rien d'autre : ouvrir le nœud (répertoire durable par OS),
écouter, se connecter, afficher le catalogue (réactif), chercher, lire avec le
lecteur natif (AVPlayer / MediaPlayerElement / GStreamer), nettoyer leurs
répertoires de lecture temporaires. Chacun offre deux vues : **Abonnements**
(par défaut, restreinte à `catalog_subscribed`) et **Explorer** (catalogue
complet, opt-in derrière un avertissement — un nœud non souscrit peut exposer
du contenu que l'utilisateur n'a pas choisi de suivre), avec désabonnement
possible depuis les deux vues. Coller un lien `champinium://channel/…` reste
une action manuelle dans l'app : l'enregistrement du scheme auprès de l'OS
(Info.plist / appxmanifest Protocol / .desktop, pour un clic direct depuis un
navigateur) est différé au packaging (Phase 6).

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
  fmt, `cargo-deny`, tests sur les 3 OS, + trois jobs de fronts (`linux-gui`,
  `macos-app`, `windows-app`) — les fronts non compilables en local sont validés
  là. Job `cold-storage` : build+clippy+test de la feature opt-in
  `cold-storage` (cœur **et** CLI), **sans** variable réseau — l'IT Arweave réel
  reste `#[ignore]`+env-gaté et n'y tourne jamais (§6, ADR 0008).
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
| IPNS | différé, adossé à bitswap (sa valeur = interop IPFS public) ; durabilité déjà couverte par le seed proactif des abonnés + les pins | ADR 0007 |
| Recherche | locale (ce que le nœud a vu) + tags DHT ; pas d'index global — assumé | risque #4, §6 |
| Persistance | seed proactif des abonnés (quota + éviction par réplication) + pins ; cold storage Arweave opt-in **livré** cœur+CLI (CS-a, feature `cold-storage`) — repli de dernier recours CID-vérifié ([ADR 0008](adr/0008-cold-storage-arweave.md)) ; fronts (CS-b) à faire | risque #1, §6 |
| NAT | relay v2 + DCUtR testés ; relays multipliables | risque #6 |
| Signature payante | palier gratuit livré ; notarisation/Authenticode différés | `packaging.md` |
| Windows/C# | validé par CI ; pas de stack intendant | `.intendant.toml` |
| Channels lot (c) | seed proactif des channels souscrits + quota + éviction + pins + retrait de seed-what-you-consume — **implémenté** (contrat FFI v7) | §6 bis |
| Channels lot (d) | denylist par clé (v2), blocage local privé de channel, purge rétroactive étendue (SeedIndex + `stop_providing`), signalements par channel — **implémenté** (contrat FFI v8) ; clôt la refonte channels (lots a–d) | §7 |

## 12. Carte des documents

- [`CLAUDE.md`](../CLAUDE.md) — principes + état d'avancement (source de vérité).
- [`AGENTS.md`](../AGENTS.md) — contrat FFI (tableau v8) + garde-fous d'équipe.
- [`docs/adr/`](adr/) — décisions : libp2p vs iroh (0001), modération côté
  nœud (0002), feeds signés (0003), transport de blocs (0006), IPNS (0007),
  stockage froid Arweave (0008)…
- [`docs/mvp-demo.md`](mvp-demo.md) / [`docs/gui-demo.md`](gui-demo.md) —
  démos de bout en bout (CLI validée ; GUI deux machines à dérouler).
- [`docs/deploy-bootstrap-relay.md`](deploy-bootstrap-relay.md) — opérer
  l'infra stateless.
- [`docs/packaging.md`](packaging.md) — distribution sans frais de signature.
