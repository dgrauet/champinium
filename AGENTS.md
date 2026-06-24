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

### Contrat actuel — v1 (`CONTRACT_VERSION = 1`)

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

Record `FfiCatalogEntry { issuer, seq, cids }`. Erreur `FfiError` (message aplati).
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
