//! Couche P2P (rust-libp2p) — Phase 1 « noyau nu ».
//!
//! Un [`Node`] encapsule un `Swarm` (TCP + Noise + Yamux) avec :
//! - **Kademlia** (provider records : qui détient quel CID) ;
//! - **identify** (échange d'adresses pour peupler la table de routage) ;
//! - **ping** (keep-alive / liveness) ;
//! - un protocole **request-response** `/champinium/block/1.0.0` pour le
//!   transfert de blocs (interim Phase 1 ; bitswap viendra plus tard).
//!
//! L'API publique ([`Node`]) parle au `Swarm` via un canal de commandes ; la
//! boucle d'évènements tourne dans une tâche tokio dédiée.

use crate::blockstore::Blockstore;
use crate::catalog::{Catalog, CatalogEntry, CatalogItem};
#[cfg(feature = "cold-storage")]
use crate::coldstore::{ArchivePayload, ArchiveQuote, ArchiveReceipt, ColdStore};
use crate::content::{cid_for, verify};
use crate::error::{CoreError, Result as CoreResult};
use crate::feed::{ChannelMeta, Feed, FeedEntry};
use crate::identity;
use crate::ingest::{self, HlsManifest, HlsSegment};
use crate::moderation::{Denylist, Moderation};
use crate::report::{Report, ReportBook};
use crate::seeding::{self, eviction_order, SeedIndex, SeededPublication};
use cid::Cid;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use libp2p::kad::{
    self,
    store::{MemoryStore, RecordStore},
    GetProvidersOk, GetRecordOk, QueryId, QueryResult, RecordKey,
};
use libp2p::request_response::{self, OutboundRequestId, ProtocolSupport};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{dcutr, gossipsub, identify, identity::Keypair, noise, ping, relay, tcp, yamux};
use libp2p::{Multiaddr, PeerId, StreamProtocol, Swarm};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
#[cfg(feature = "cold-storage")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

const BLOCK_PROTOCOL: &str = "/champinium/block/1.0.0";
const IDENTIFY_PROTOCOL: &str = "/champinium/0.1.0";
const FEEDS_TOPIC: &str = "champinium/feeds/v1";
const REPORTS_TOPIC: &str = "champinium/reports/v1";

/// Taille max d'un bloc servi via request-response (un segment HLS = un bloc).
/// Le défaut cbor (10 MiB) est trop bas pour un segment vidéo haute qualité.
const MAX_BLOCK_SIZE: u64 = 64 * 1024 * 1024;
/// Taille max d'une requête de bloc (un CID) — le défaut (1 MiB) suffit large.
const MAX_BLOCK_REQUEST_SIZE: u64 = 4 * 1024;
/// Taille max d'un feed diffusé en gossipsub. Le défaut (64 KiB) plafonne à
/// ~1000 CIDs ; on vise beaucoup plus haut pour un créateur prolifique.
const MAX_FEED_SIZE: usize = 4 * 1024 * 1024;
/// Nombre max de provider records annoncés (le défaut 1024 est atteint par un
/// seeder réel — des heures de vidéo = des milliers de segments).
const MAX_PROVIDED_KEYS: usize = 1_000_000;
/// Nombre max de records DHT stockés localement (feeds d'autres créateurs).
const MAX_DHT_RECORDS: usize = 100_000;
/// Intervalle du suivi actif périodique des channels souscrits (spec channels
/// §2). Surchargeable en test via
/// [`Node::with_moderation_and_follow_interval`] — **pas** via un setter
/// post-construction : `follow_loop` lit `follow_interval` une seule fois par
/// itération, sans jamais céder la main entre temps (zéro `.await`) avant de
/// s'engager sur un `sleep`, donc un setter appelé après coup entre en course
/// avec la toute première lecture. Sous charge (CI), cette course a été
/// perdue en pratique : le `sleep` se figeait sur cette constante de 5 min et
/// le test expirait. D'où l'intervalle passé **au constructeur**, effectif
/// avant même le `tokio::spawn` de la boucle — aucune course possible.
const FOLLOW_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// Intervalle du seed proactif des channels souscrits (lot c) : à chaque tic
/// (et à chaque évènement catalogue, cf. `seed_loop`), passe en revue les
/// channels souscrits et complète leurs publications manquantes sous quota.
/// Même leçon que [`FOLLOW_INTERVAL`] : surchargeable au constructeur
/// ([`Node::with_moderation_and_intervals`]), jamais par un setter posé après
/// le `tokio::spawn`.
const SEED_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Demande d'un bloc par CID (octets du CID).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockRequest(pub Vec<u8>);

/// Réponse : le bloc s'il est détenu, sinon `None`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockResponse(pub Option<Vec<u8>>);

#[derive(NetworkBehaviour)]
struct Behaviour {
    kademlia: kad::Behaviour<MemoryStore>,
    identify: identify::Behaviour,
    ping: ping::Behaviour,
    blocks: request_response::cbor::Behaviour<BlockRequest, BlockResponse>,
    gossipsub: gossipsub::Behaviour,
    // NAT traversal : client de circuit relay v2 + hole punching DCUtR.
    relay_client: relay::client::Behaviour,
    dcutr: dcutr::Behaviour,
}

impl Behaviour {
    fn new(key: &Keypair, relay_client: relay::client::Behaviour) -> Self {
        let peer_id = key.public().to_peer_id();
        // Filtrage des stores entrants : sans lui, n'importe quel pair peut
        // écraser le record de feed d'autrui chez les nœuds stockeurs (déni de
        // découverte). Les records sont validés dans la boucle d'évènements
        // (voir `EventLoop::handle_inbound_kad_request`).
        let mut kad_cfg = kad::Config::default();
        kad_cfg.set_record_filtering(kad::StoreInserts::FilterBoth);
        let store_cfg = kad::store::MemoryStoreConfig {
            max_provided_keys: MAX_PROVIDED_KEYS,
            max_records: MAX_DHT_RECORDS,
            max_value_bytes: MAX_FEED_SIZE,
            ..Default::default()
        };
        let store = MemoryStore::with_config(peer_id, store_cfg);
        let kademlia = kad::Behaviour::with_config(peer_id, store, kad_cfg);
        let identify = identify::Behaviour::new(identify::Config::new(
            IDENTIFY_PROTOCOL.to_string(),
            key.public(),
        ));
        // Codec cbor avec des plafonds relevés pour la cible média (segments HLS).
        let block_codec = request_response::cbor::codec::Codec::default()
            .set_request_size_maximum(MAX_BLOCK_REQUEST_SIZE)
            .set_response_size_maximum(MAX_BLOCK_SIZE);
        let blocks = request_response::Behaviour::with_codec(
            block_codec,
            [(StreamProtocol::new(BLOCK_PROTOCOL), ProtocolSupport::Full)],
            request_response::Config::default(),
        );
        // Feeds signés diffusés en gossipsub. Messages signés par l'identité
        // libp2p. `validate_messages` : un message n'est RELAYÉ qu'après que la
        // couche applicative a rapporté son verdict (feed vérifié) — voir
        // `EventLoop::handle_feed_message` ; sans rapport, plus rien ne circule.
        let gossipsub_cfg = gossipsub::ConfigBuilder::default()
            .max_transmit_size(MAX_FEED_SIZE)
            .validate_messages()
            .build()
            .expect("config gossipsub valide");
        let mut gossipsub = gossipsub::Behaviour::new(
            gossipsub::MessageAuthenticity::Signed(key.clone()),
            gossipsub_cfg,
        )
        .expect("config gossipsub valide");
        // Peer scoring : chaque feed invalide rapporté (Reject) dégrade le score
        // de son émetteur ; sous les seuils par défaut, ses messages ne sont
        // plus relayés puis le pair est graylisté. C'est la brique de
        // réputation contre l'inondation du catalogue par des clés jetables.
        let mut score_params = gossipsub::PeerScoreParams::default();
        for topic in [FEEDS_TOPIC, REPORTS_TOPIC] {
            score_params.topics.insert(
                gossipsub::IdentTopic::new(topic).hash(),
                gossipsub::TopicScoreParams::default(),
            );
        }
        gossipsub
            .with_peer_score(score_params, gossipsub::PeerScoreThresholds::default())
            .expect("paramètres de scoring valides");
        Self {
            kademlia,
            identify,
            ping: ping::Behaviour::default(),
            blocks,
            gossipsub,
            relay_client,
            dcutr: dcutr::Behaviour::new(peer_id),
        }
    }
}

/// Commandes envoyées à la boucle d'évènements.
enum Command {
    Listen {
        addr: Multiaddr,
        tx: oneshot::Sender<CoreResult<Multiaddr>>,
    },
    Dial {
        addr: Multiaddr,
        tx: oneshot::Sender<CoreResult<()>>,
    },
    AddAddress {
        peer: PeerId,
        addr: Multiaddr,
    },
    Provide {
        key: RecordKey,
        tx: oneshot::Sender<CoreResult<()>>,
    },
    GetProviders {
        key: RecordKey,
        tx: oneshot::Sender<HashSet<PeerId>>,
    },
    RequestBlock {
        peer: PeerId,
        cid: Cid,
        tx: oneshot::Sender<CoreResult<Vec<u8>>>,
    },
    ListenAddrs {
        tx: oneshot::Sender<Vec<Multiaddr>>,
    },
    PublishFeed {
        data: Vec<u8>,
        tx: oneshot::Sender<CoreResult<()>>,
    },
    PutRecord {
        key: RecordKey,
        value: Vec<u8>,
        tx: oneshot::Sender<CoreResult<()>>,
    },
    GetRecord {
        key: RecordKey,
        tx: oneshot::Sender<Vec<Vec<u8>>>,
    },
    PeerScore {
        peer: PeerId,
        tx: oneshot::Sender<Option<f64>>,
    },
    PublishReport {
        data: Vec<u8>,
        tx: oneshot::Sender<CoreResult<()>>,
    },
    /// Arrête d'annoncer ce nœud comme fournisseur d'un CID (purge de
    /// modération par clé, tâche d/2 — voir `Node::stop_providing`).
    /// Fire-and-forget : pas de réponse attendue, best-effort comme
    /// `Provide`/`Command::Provide` côté annonce.
    StopProviding {
        key: RecordKey,
    },
}

/// Politique de stockage appliquée par [`Node::get_with`] lors d'une
/// récupération réseau (retrait de seed-what-you-consume, spec channels
/// lot c) : un hit local est toujours servi tel quel et le checkpoint
/// modération #2 s'applique dans les deux cas — seul le sort du bloc
/// *nouvellement récupéré depuis le réseau* diffère.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StorePolicy {
    /// Ancien comportement : le bloc est mis en cache ET réannoncé (le nœud
    /// devient fournisseur). Réservé aux cas où le nœud choisit explicitement
    /// de seeder ce contenu (channel souscrit, ingestion, réparation d'un
    /// cache local corrompu).
    Seed,
    /// Rien n'est écrit ni annoncé : les octets sont rendus à l'appelant et
    /// oubliés. C'est le nouveau défaut de [`Node::get`] — consommer un
    /// contenu ne doit plus, par défaut, engager le nœud à le seeder.
    Stream,
}

/// Poignée vers un nœud P2P en fonctionnement.
#[derive(Clone)]
pub struct Node {
    peer_id: PeerId,
    keypair: Keypair,
    blockstore: Blockstore,
    moderation: Arc<RwLock<Moderation>>,
    catalog: Arc<Mutex<Catalog>>,
    catalog_events: tokio::sync::broadcast::Sender<()>,
    reports: Arc<Mutex<ReportBook>>,
    feed_seq: Arc<Mutex<u64>>,
    channel_profile: Arc<Mutex<ChannelMeta>>,
    /// Abonnements locaux (channels suivis) — état **privé** du nœud, jamais
    /// publié sur le réseau (spec channels §2).
    subscriptions: Arc<Mutex<BTreeSet<PeerId>>>,
    /// Index de seed proactif (spec channels lot c) : publications retenues
    /// par émetteur, pins, quota — voir [`crate::seeding`].
    seed_index: Arc<Mutex<SeedIndex>>,
    /// Quota de seed courant (octets), persisté à côté de l'index.
    seed_quota: Arc<Mutex<u64>>,
    /// Notifie chaque publication nouvellement seedée ou purgée (FFI listener
    /// à venir tâche 5).
    seed_events: tokio::sync::broadcast::Sender<()>,
    /// Réveil best-effort de `seed_loop` (ex. changement de quota) — distinct
    /// de `catalog_events` : un changement de quota n'est pas un changement de
    /// catalogue.
    seed_wake: tokio::sync::broadcast::Sender<()>,
    /// Channels bloqués LOCALEMENT (tâche 3) : préférence strictement privée
    /// de ce nœud, jamais publiée ni signalée sur le réseau (même patron que
    /// `subscriptions`, dotfile `.blocked_channels`). Fusionné avec les clés
    /// bannies par denylist (`Moderation::is_blocked_key`) pour l'enforcement
    /// aux mêmes checkpoints (voir `is_key_blocked_inner`).
    blocked_channels: Arc<Mutex<BTreeSet<PeerId>>>,
    /// Marqueur de vivacité : la tâche de suivi périodique n'en tient qu'un
    /// [`Weak`], ce qui lui permet de s'arrêter dès que toutes les poignées
    /// `Node` (qui clonent ce `Arc`) sont tombées, sans jamais retenir un
    /// `Node` fort qui empêcherait la boucle d'évènements de s'arrêter. Jamais
    /// lu directement : seul son compteur de références (via `Clone`) importe.
    #[allow(dead_code)]
    alive: Arc<()>,
    cmd_tx: mpsc::Sender<Command>,
    /// Backend de repli de récupération froide (ADR 0008, CS-a tâche 3) —
    /// `None` par défaut : aucun câblage de production dans cette tâche, seule
    /// l'injection de test (`with_cold_for_tests`) le peuple. Gaté par la
    /// feature `cold-storage` : absent de la struct (et donc du binaire) dans
    /// un build par défaut.
    #[cfg(feature = "cold-storage")]
    cold: Option<Arc<dyn ColdStore>>,
    /// Débrayage du repli froid, persisté (dotfile `.cold_enabled`) — un
    /// utilisateur peut vouloir désactiver l'appel réseau au froid (coût,
    /// vie privée) sans recompiler. Défaut vrai (absent/corrompu → activé).
    #[cfg(feature = "cold-storage")]
    cold_retrieval_enabled: Arc<AtomicBool>,
    /// Portefeuille Arweave (JWK, fourni par le créateur) utilisé pour signer
    /// les transactions d'archivage — `None` par défaut : comme `cold`, aucun
    /// câblage de production dans cette tâche, seule l'injection de test
    /// (`with_cold_and_wallet_for_tests`) le peuple. L'archivage est réservé au
    /// propre contenu du créateur et exige à la fois `cold` et `wallet`.
    #[cfg(feature = "cold-storage")]
    wallet: Option<crate::coldstore::ArweaveWallet>,
}

/// Aperçu d'un channel résolu par lien (spec 2026-07-23, partie A), voir
/// [`Node::resolve_channel`]. Instantané en lecture seule — ne crée ni
/// n'implique aucun abonnement.
#[derive(Debug, Clone)]
pub struct ChannelPreview {
    pub issuer: PeerId,
    pub channel: ChannelMeta,
    pub items: Vec<CatalogItem>,
    pub subscribed: bool,
    pub blocked: bool,
}

/// Convertit les entrées d'un [`Feed`] en [`CatalogItem`] — même règle que
/// [`Catalog::entries`] (CID invalide silencieusement ignoré), dupliquée ici
/// car `resolve_channel` doit pouvoir construire un aperçu SANS passer par le
/// catalogue (chemin clé bloquée, voir sa doc).
fn catalog_items_from_feed(feed: &Feed) -> Vec<CatalogItem> {
    feed.entries
        .iter()
        .filter_map(|e| {
            e.cid.parse::<Cid>().ok().map(|cid| CatalogItem {
                cid,
                title: e.title.clone(),
                tags: e.tags.clone(),
            })
        })
        .collect()
}

impl Node {
    /// Construit un nœud avec la modération par défaut active (non désactivable).
    pub async fn new(keypair: Keypair, blockstore: Blockstore) -> CoreResult<Self> {
        Self::with_moderation(keypair, blockstore, Moderation::with_default()?).await
    }

    /// Ouvre (ou crée) un nœud sous `data_dir` : identité Ed25519 persistée +
    /// magasin de blocs, avec la modération par défaut active. Point d'entrée
    /// commun aux fronts (via FFI) et aux consommateurs Rust directs (GTK).
    pub async fn open(data_dir: &Path) -> CoreResult<Self> {
        let keypair = identity::load_or_generate(data_dir.join("node.key"))?;
        let blockstore = Blockstore::open(data_dir.join("blocks"))?;
        Self::new(keypair, blockstore).await
    }

    /// Se connecte à un pair désigné par une multiaddr terminée par
    /// `/p2p/<peerid>` : enregistre son adresse dans la table de routage puis
    /// compose. Logique d'orchestration partagée (ne pas dupliquer côté front).
    pub async fn connect(&self, addr: Multiaddr) -> CoreResult<()> {
        let (peer, base) = split_peer_id(addr)?;
        self.add_address(peer, base.clone()).await?;
        self.dial(base).await
    }

    /// Construit un nœud avec un moteur de modération explicite (tests, opérateurs
    /// qui souscrivent à des denylists tierces). Démarre la boucle d'évènements.
    pub async fn with_moderation(
        keypair: Keypair,
        blockstore: Blockstore,
        moderation: Moderation,
    ) -> CoreResult<Self> {
        Self::with_moderation_and_follow_interval(keypair, blockstore, moderation, FOLLOW_INTERVAL)
            .await
    }

    /// Comme [`Node::with_moderation`], mais avec un `follow_interval` de
    /// suivi actif explicite plutôt que la constante de production
    /// [`FOLLOW_INTERVAL`] (5 min, bien trop longue pour un test). Le
    /// `seed_interval` (lot c) reste la constante de production
    /// [`SEED_INTERVAL`] — utiliser [`Node::with_moderation_and_intervals`]
    /// pour le surcharger aussi.
    #[doc(hidden)]
    pub async fn with_moderation_and_follow_interval(
        keypair: Keypair,
        blockstore: Blockstore,
        moderation: Moderation,
        follow_interval: Duration,
    ) -> CoreResult<Self> {
        Self::with_moderation_and_intervals(
            keypair,
            blockstore,
            moderation,
            follow_interval,
            SEED_INTERVAL,
        )
        .await
    }

    /// Comme [`Node::with_moderation`], mais avec `follow_interval` (suivi de
    /// channel) ET `seed_interval` (seed proactif, lot c) explicites plutôt
    /// que les constantes de production [`FOLLOW_INTERVAL`]/[`SEED_INTERVAL`]
    /// (5 min chacune, bien trop long pour un test). Réservé aux tests :
    /// contrairement à un setter post-construction (qui entrerait en course
    /// avec la toute première lecture de l'intervalle par la boucle
    /// correspondante — voir le commentaire sur `FOLLOW_INTERVAL`), les deux
    /// intervalles sont effectifs **avant** le `tokio::spawn` de leur boucle,
    /// donc sans course possible.
    #[doc(hidden)]
    pub async fn with_moderation_and_intervals(
        keypair: Keypair,
        blockstore: Blockstore,
        moderation: Moderation,
        follow_interval: Duration,
        seed_interval: Duration,
    ) -> CoreResult<Self> {
        let peer_id = identity::peer_id(&keypair);
        let mut swarm = build_swarm(keypair.clone())?;
        // Mode serveur : stocke et sert les provider records (pas seulement client).
        swarm
            .behaviour_mut()
            .kademlia
            .set_mode(Some(kad::Mode::Server));
        // Souscription aux topics des feeds et des signalements.
        let topic = gossipsub::IdentTopic::new(FEEDS_TOPIC);
        let reports_topic = gossipsub::IdentTopic::new(REPORTS_TOPIC);
        for t in [&topic, &reports_topic] {
            swarm
                .behaviour_mut()
                .gossipsub
                .subscribe(t)
                .map_err(|e| CoreError::Network(format!("gossipsub subscribe: {e}")))?;
        }

        let moderation = Arc::new(RwLock::new(moderation));
        let catalog = Arc::new(Mutex::new(Catalog::new()));
        let reports = Arc::new(Mutex::new(ReportBook::default()));
        // Abonnements locaux persistés (spec channels §2) : rechargés avant le
        // démarrage de la boucle de suivi pour que le passage initial les couvre.
        let subscriptions = Arc::new(Mutex::new(load_subscriptions(&blockstore)));
        // Channels bloqués localement (tâche 3) : rechargés avant le démarrage
        // de la boucle de gossip (le check d'ingestion catalogue doit en tenir
        // compte dès le premier feed reçu).
        let blocked_channels = Arc::new(Mutex::new(load_blocked_channels(&blockstore)));
        // Canal d'événements « catalogue mis à jour » : capacité large car un
        // abonné lent ne doit pas perdre le signal (les tics sont fusionnables :
        // rater un tic mais en recevoir un plus tard suffit à se resynchroniser).
        let (catalog_events, _) = tokio::sync::broadcast::channel(64);
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let event_loop = EventLoop::new(
            swarm,
            blockstore.clone(),
            moderation.clone(),
            catalog.clone(),
            catalog_events.clone(),
            reports.clone(),
            subscriptions.clone(),
            blocked_channels.clone(),
            topic,
            reports_topic,
            cmd_rx,
        );
        tokio::spawn(event_loop.run());

        // Reprend le seq de feed là où il s'était arrêté (sinon un nœud redémarré
        // republierait un seq plus petit, ignoré par le LWW des catalogues pairs).
        let feed_seq = Arc::new(Mutex::new(load_feed_seq(&blockstore)));
        // Recharge le profil de channel persisté (identité éditoriale, spec
        // channels §1) : un profil corrompu ne doit pas empêcher le démarrage.
        let channel_profile = Arc::new(Mutex::new(load_channel_profile(&blockstore)));

        // Index de seed proactif (lot c) : rechargé depuis le disque (défaut
        // vide/quota par défaut si absent — un index/quota corrompu ne doit
        // pas empêcher le démarrage, même logique que le profil de channel).
        let seed_index = Arc::new(Mutex::new(seeding::load_seed_index(&blockstore)));
        let seed_quota = Arc::new(Mutex::new(seeding::load_seed_quota(&blockstore)));
        let (seed_events, _) = tokio::sync::broadcast::channel(64);
        let (seed_wake, _) = tokio::sync::broadcast::channel(16);

        let alive = Arc::new(());

        // Suivi actif périodique des channels souscrits (spec channels §2) : ne
        // tient qu'un `Weak<()>` — dès que la dernière poignée `Node` tombe,
        // `alive_weak.upgrade()` échoue et la boucle s'arrête, libérant son
        // clone de `cmd_tx` (sinon la boucle d'évènements ne s'arrêterait
        // jamais : `cmd_rx.recv()` ne renvoie `None` que quand tous les
        // émetteurs sont tombés). `follow_interval` est fixé une fois pour
        // toutes ici, avant ce spawn — voir le commentaire sur
        // [`FOLLOW_INTERVAL`] pour la course que ça évite.
        tokio::spawn(follow_loop(
            Arc::downgrade(&alive),
            cmd_tx.clone(),
            catalog.clone(),
            catalog_events.clone(),
            subscriptions.clone(),
            moderation.clone(),
            blocked_channels.clone(),
            follow_interval,
        ));

        // Seed proactif des channels souscrits (lot c) : même patron de
        // vivacité que `follow_loop` (Weak, jamais un `Node` fort). Se
        // réveille aussi sur `catalog_events` (nouvelle publication à seeder
        // au plus vite) et sur `seed_wake` (ex. changement de quota).
        tokio::spawn(seed_loop(
            Arc::downgrade(&alive),
            SeedLoopState {
                cmd_tx: cmd_tx.clone(),
                blockstore: blockstore.clone(),
                moderation: moderation.clone(),
                catalog: catalog.clone(),
                subscriptions: subscriptions.clone(),
                seed_index: seed_index.clone(),
                seed_quota: seed_quota.clone(),
                seed_events: seed_events.clone(),
                reports: reports.clone(),
                keypair: keypair.clone(),
                peer_id,
                quota_blocked: Arc::new(Mutex::new(HashSet::new())),
            },
            catalog_events.subscribe(),
            seed_wake.subscribe(),
            seed_interval,
        ));

        // Repli froid (CS-a tâche 3) : recharge le débrayage persisté (défaut
        // activé si absent/corrompu, même patron que le profil de channel ou
        // l'index de seed — un dotfile illisible ne doit jamais empêcher le
        // démarrage du nœud). Aucun backend réel câblé ici : `cold` reste
        // `None` tant qu'aucun appelant n'utilise `with_cold_for_tests`.
        #[cfg(feature = "cold-storage")]
        let cold_retrieval_enabled = Arc::new(AtomicBool::new(load_cold_enabled(&blockstore)));

        Ok(Self {
            peer_id,
            keypair,
            blockstore,
            moderation,
            catalog,
            catalog_events,
            reports,
            feed_seq,
            channel_profile,
            subscriptions,
            seed_index,
            seed_quota,
            seed_events,
            seed_wake,
            blocked_channels,
            alive,
            cmd_tx,
            #[cfg(feature = "cold-storage")]
            cold: None,
            #[cfg(feature = "cold-storage")]
            cold_retrieval_enabled,
            #[cfg(feature = "cold-storage")]
            wallet: None,
        })
    }

    /// Nombre de rapporteurs **distincts** ayant signalé ce CID (agrégat local,
    /// borné). Matière première pour un éditeur de denylist — aucun effet
    /// automatique sur le contenu.
    pub fn report_count(&self, cid: &Cid) -> usize {
        self.reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .count(cid)
    }

    /// CIDs signalés avec leur nombre de rapporteurs distincts.
    pub fn report_counts(&self) -> Vec<(Cid, usize)> {
        self.reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .counts()
    }

    /// Signalements agrégés **par émetteur** (spec channels lot d, tâche 4) :
    /// jointure locale entre l'agrégat de rapports existant
    /// ([`Node::report_counts`]) et le mapping CID→émetteur du catalogue
    /// reconstruit localement. Pour chaque émetteur retenu :
    /// `(rapporteurs distincts cumulés sur tous ses CIDs signalés, nombre de
    /// CIDs distincts signalés qui lui sont attribués)`.
    ///
    /// **Limite assumée** : un CID signalé qui n'apparaît dans AUCUNE entrée
    /// du catalogue local (émetteur pas vu par ce nœud, ou entrée expirée)
    /// n'apparaît PAS dans ce résultat — il reste compté uniquement côté
    /// [`Node::report_counts`] (agrégat global, sans attribution). Lecture
    /// seule, aucun effet réseau.
    pub fn report_counts_by_channel(&self) -> Vec<(PeerId, u64, u64)> {
        let cid_issuer: HashMap<Cid, PeerId> = self
            .catalog_entries()
            .into_iter()
            .flat_map(|entry| {
                let issuer = entry.issuer;
                entry.cids.into_iter().map(move |cid| (cid, issuer))
            })
            .collect();

        let mut by_channel: HashMap<PeerId, (u64, u64)> = HashMap::new();
        for (cid, reporters) in self.report_counts() {
            let Some(issuer) = cid_issuer.get(&cid) else {
                continue;
            };
            let tally = by_channel.entry(*issuer).or_insert((0, 0));
            tally.0 += reporters as u64;
            tally.1 += 1;
        }
        by_channel
            .into_iter()
            .map(|(issuer, (reporters, cids))| (issuer, reporters, cids))
            .collect()
    }

    /// Score gossipsub d'un pair (réputation ; `None` si le pair est inconnu
    /// de la couche gossip). Diagnostic/observabilité — les seuils d'application
    /// (arrêt du relais, graylist) sont gérés par gossipsub lui-même.
    pub async fn gossip_peer_score(&self, peer: PeerId) -> CoreResult<Option<f64>> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::PeerScore { peer, tx }).await?;
        rx.await.map_err(|_| CoreError::Shutdown)
    }

    /// S'abonne aux mises à jour du catalogue : un tic est émis à chaque
    /// changement effectif (feed gossip appliqué, publication locale, feed DHT).
    /// Les tics sont fusionnables : un abonné qui prend du retard peut en
    /// manquer, un tic ultérieur suffit à relire l'instantané (`catalog_entries`).
    pub fn subscribe_catalog(&self) -> tokio::sync::broadcast::Receiver<()> {
        self.catalog_events.subscribe()
    }

    /// S'abonne aux évènements de seed proactif (lot c) : un tic est émis à
    /// chaque publication nouvellement seedée ou purgée (désabonnement). Le
    /// listener FFI viendra en tâche 5 ; ce canal est le point d'accroche.
    pub fn subscribe_seed(&self) -> tokio::sync::broadcast::Receiver<()> {
        self.seed_events.subscribe()
    }

    /// `PeerId` du nœud.
    pub fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    /// Accès au magasin de blocs local.
    pub fn blockstore(&self) -> &Blockstore {
        &self.blockstore
    }

    /// Écoute sur `addr` ; renvoie l'adresse effectivement liée.
    pub async fn listen(&self, addr: Multiaddr) -> CoreResult<Multiaddr> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::Listen { addr, tx }).await?;
        rx.await.map_err(|_| CoreError::Shutdown)?
    }

    /// Adresses d'écoute actuelles.
    pub async fn listen_addrs(&self) -> CoreResult<Vec<Multiaddr>> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::ListenAddrs { tx }).await?;
        rx.await.map_err(|_| CoreError::Shutdown)
    }

    /// Compose vers un pair.
    pub async fn dial(&self, addr: Multiaddr) -> CoreResult<()> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::Dial { addr, tx }).await?;
        rx.await.map_err(|_| CoreError::Shutdown)?
    }

    /// Enregistre une adresse connue pour un pair (table de routage Kademlia).
    pub async fn add_address(&self, peer: PeerId, addr: Multiaddr) -> CoreResult<()> {
        self.send(Command::AddAddress { peer, addr }).await
    }

    /// Indique si un CID est actuellement bloqué par la modération.
    pub fn is_blocked(&self, cid: &Cid) -> bool {
        self.moderation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_blocked(cid)
    }

    /// Nombre de CIDs actuellement bloqués.
    pub fn blocked_count(&self) -> usize {
        self.moderation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Souscrit **à chaud** à une denylist signée : vérifie la signature, ajoute
    /// ses CIDs ET ses clés au moteur, puis **purge rétroactivement** :
    /// - tout bloc du magasin dont le CID est directement listé (comportement
    ///   historique, denylist v1/v2 par CID) ;
    /// - tout émetteur dont la clé est désormais bannie (v2, finding M2) :
    ///   son entrée de catalogue, ses publications retenues (SeedIndex —
    ///   modération prime sur les pins, `keep_pinned=false`) et leurs blocs
    ///   sont purgés, et le nœud arrête d'en annoncer chacun comme
    ///   fournisseur (`Node::stop_providing`).
    ///
    /// Renvoie le nombre total de blocs supprimés du magasin.
    pub async fn subscribe_denylist(&self, list: &Denylist) -> CoreResult<usize> {
        {
            let mut mod_guard = self
                .moderation
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            mod_guard.subscribe(list)?;
        }

        // --- Purge par CLÉ (finding M2) : toute entrée de catalogue dont
        // l'émetteur est désormais bloqué — pas seulement les clés de CETTE
        // liste : une clé a pu être bloquée par une souscription antérieure
        // dont le catalogue vient tout juste d'être peuplé.
        let mut purged = 0usize;
        let blocked_entries: Vec<CatalogEntry> = self
            .catalog_entries()
            .into_iter()
            .filter(|e| is_key_blocked_inner(&self.moderation, &self.blocked_channels, &e.issuer))
            .collect();
        for entry in &blocked_entries {
            purged += self.purge_blocked_issuer(entry.issuer, &entry.cids).await;
        }
        if !blocked_entries.is_empty() {
            let _ = self.catalog_events.send(());
        }

        // --- Purge par CID (comportement historique, v1/v2) : les blocs
        // encore présents et directement listés (les blocs d'émetteurs
        // bloqués ci-dessus ont déjà été retirés, donc jamais recomptés ici).
        for cid in self.blockstore.list()? {
            if self.is_blocked(&cid) {
                self.blockstore.remove(&cid)?;
                self.stop_providing(cid).await;
                purged += 1;
            }
        }
        Ok(purged)
    }

    /// Purge toutes les traces locales ATTRIBUÉES à un émetteur devenu bloqué
    /// (denylist, tâche 2 ; ou blocage local de channel, tâche 3) : retire son
    /// entrée du catalogue, purge le SeedIndex de ses publications
    /// (`keep_pinned=false` : la modération prime sur les pins, contrairement
    /// à `unsubscribe` qui les préserve), supprime du magasin tout bloc
    /// devenu orphelin, puis arrête d'en annoncer chacun comme fournisseur.
    ///
    /// Ne purge QUE ce que ce nœud a lui-même attribué à cet émetteur (son
    /// entrée de catalogue au moment de l'appel + ce que son SeedIndex avait
    /// retenu en son nom) — jamais un contenu tiers découvert en le
    /// mentionnant (revue post-implémentation, finding critique I2 : une clé
    /// bannie ne doit pas pouvoir faire disparaître le contenu d'autrui en le
    /// listant dans son propre feed, ce serait de la censure par injection).
    ///
    /// GUARD (finding I1) : un bloc encore référencé par la publication d'un
    /// AUTRE émetteur (segment partagé, cf. lot c) survit — la suppression
    /// passe par [`remove_unshared_blocks`], la même garde que `unsubscribe`.
    /// Renvoie le nombre de blocs effectivement supprimés du magasin.
    async fn purge_blocked_issuer(&self, issuer: PeerId, entry_cids: &[Cid]) -> usize {
        self.catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove_issuer(&issuer);

        let evicted = {
            let mut idx = self
                .seed_index
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let evicted = idx.purge_issuer(&issuer.to_string(), false);
            if !evicted.is_empty() {
                if let Err(e) = seeding::save_seed_index(&self.blockstore, &idx) {
                    tracing::warn!("persistance de l'index de seed échouée: {e}");
                }
            }
            evicted
        };

        // Candidats à la purge : les publications retenues par le SeedIndex
        // (manifeste + segments) ET les CIDs connus du catalogue au moment de
        // l'appel (le feed peut référencer un manifeste jamais réellement
        // seedé chez ce nœud) — tous deux strictement ATTRIBUÉS à `issuer`.
        let mut publications = evicted.clone();
        for cid in entry_cids {
            let manifest_cid = cid.to_string();
            if !publications.iter().any(|p| p.manifest_cid == manifest_cid) {
                publications.push(SeededPublication {
                    manifest_cid,
                    segment_cids: Vec::new(),
                    total_bytes: 0,
                    order: 0,
                });
            }
        }

        // Instantané post-retrait du SeedIndex : un CID encore dedans
        // appartient à une publication d'un AUTRE émetteur (partagée) et ne
        // doit ni être purgé, ni voir son annonce de fourniture arrêtée —
        // aucun `.await` entre cette lecture et `remove_unshared_blocks`
        // ci-dessous, qui relit le même état : pas de course possible.
        let remaining: HashSet<String> = self
            .seed_index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .all_cids()
            .into_iter()
            .collect();
        let mut candidate_cids: HashSet<Cid> = HashSet::new();
        for p in &publications {
            if let Ok(cid) = p.manifest_cid.parse::<Cid>() {
                candidate_cids.insert(cid);
            }
            for seg in &p.segment_cids {
                if let Ok(cid) = seg.parse::<Cid>() {
                    candidate_cids.insert(cid);
                }
            }
        }
        let mut count = 0usize;
        for cid in &candidate_cids {
            if remaining.contains(&cid.to_string()) {
                continue; // partagé avec un émetteur non bloqué : on ne purge pas.
            }
            if self.blockstore.has(cid) {
                count += 1;
            }
            // Retrait passif de l'annonce de fourniture (M3) : ce nœud
            // arrête simplement de se déclarer fournisseur DHT de ce CID —
            // aucune émission réseau, aucun rapport, rien de signalé aux
            // pairs (cohérent avec le caractère strictement local du
            // blocage tâche 3, et sobre pour la tâche 2).
            self.stop_providing(*cid).await;
        }
        remove_unshared_blocks(&self.blockstore, &self.seed_index, &publications);

        if !evicted.is_empty() {
            let _ = self.seed_events.send(());
        }
        count
    }

    /// Arrête d'annoncer ce nœud comme fournisseur d'un CID (purge de
    /// modération, tâches 2/3). `libp2p-kad` 0.48 (utilisé par libp2p 0.56)
    /// expose `Behaviour::stop_providing` : le provider record LOCAL est
    /// retiré immédiatement. Limite assumée : les copies déjà propagées chez
    /// des pairs distants ne sont pas rappelées — elles s'éteignent à leur
    /// propre expiration TTL (mêmes garanties best-effort que `provide`/
    /// `Command::Provide`, pas de garantie de retrait réseau instantané).
    async fn stop_providing(&self, cid: Cid) {
        let _ = self
            .cmd_tx
            .send(Command::StopProviding {
                key: RecordKey::new(&cid.to_bytes()),
            })
            .await;
    }

    /// Publie un feed signé listant `cids` (sans métadonnées) : voir
    /// [`Node::publish_feed_with`].
    pub async fn publish_feed(&self, cids: &[Cid]) -> CoreResult<()> {
        let entries: Vec<FeedEntry> = cids
            .iter()
            .map(|c| FeedEntry {
                cid: c.to_string(),
                title: String::new(),
                tags: Vec::new(),
            })
            .collect();
        self.publish_feed_with(&entries).await
    }

    /// Publie un feed v2 signé avec métadonnées (titre, tags) : l'ajoute au
    /// catalogue local, le diffuse en gossipsub, le PUT dans la DHT, et
    /// **s'annonce fournisseur de chaque tag** (`/champinium/tag/<tag>`) pour la
    /// découverte par tag hors gossip. Le `seq` est incrémenté à chaque appel.
    pub async fn publish_feed_with(&self, entries: &[FeedEntry]) -> CoreResult<()> {
        // Incrément et persistance sous le même verrou : deux publications
        // concurrentes ne peuvent pas réordonner le seq écrit sur disque (sinon,
        // au redémarrage, on republierait un seq déjà vu, ignoré par le LWW).
        let seq = {
            let mut guard = self
                .feed_seq
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *guard += 1;
            if let Err(e) = std::fs::write(feed_seq_path(&self.blockstore), guard.to_string()) {
                tracing::warn!("persistance du seq de feed échouée: {e}");
            }
            *guard
        };
        let profile = self.channel_profile();
        let feed = Feed::build_signed_with(&self.keypair, seq, &profile, entries)?;
        // Le créateur figure dans son propre catalogue.
        let subs = self.subscriptions_snapshot();
        let changed = self
            .catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .apply(feed.clone(), &subs)?;
        if changed {
            let _ = self.catalog_events.send(());
        }
        let data = feed.to_json()?.into_bytes();
        // PUT du feed dans la DHT (best-effort) : permet la découverte hors gossip.
        let (put_tx, _put_rx) = oneshot::channel();
        let _ = self
            .cmd_tx
            .send(Command::PutRecord {
                key: feed_record_key(&self.peer_id),
                value: data.clone(),
                tx: put_tx,
            })
            .await;
        // Annonce des tags (best-effort, plafonné) : le fournisseur du tag est
        // l'émetteur lui-même ; un chercheur récupère ensuite son feed SIGNÉ et
        // filtre — un tag annoncé à tort ne fait perdre qu'une requête.
        for tag in feed.all_tags().into_iter().take(MAX_PROVIDED_TAGS) {
            let (tx, _rx) = oneshot::channel();
            let _ = self
                .cmd_tx
                .send(Command::Provide {
                    key: tag_provider_key(&tag),
                    tx,
                })
                .await;
        }
        // Diffusion live en gossipsub.
        let (tx, rx) = oneshot::channel();
        self.send(Command::PublishFeed { data, tx }).await?;
        rx.await.map_err(|_| CoreError::Shutdown)?
    }

    /// Recherche **locale** dans le catalogue reconstruit (titres, tags).
    /// Limite assumée : ne couvre que les feeds que ce nœud a vus passer.
    pub fn search(&self, query: &str) -> Vec<crate::catalog::SearchHit> {
        self.catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .search(query)
    }

    /// Recherche par tag **via la DHT** (hors gossip) : retrouve les émetteurs
    /// annoncés fournisseurs du tag, récupère et vérifie leurs feeds signés,
    /// alimente le catalogue, et renvoie les contenus portant ce tag.
    pub async fn search_tag(&self, tag: &str) -> CoreResult<Vec<crate::catalog::SearchHit>> {
        let tag = crate::feed::normalize_tag(tag);
        if tag.is_empty() {
            return Ok(Vec::new());
        }
        let (tx, rx) = oneshot::channel();
        self.send(Command::GetProviders {
            key: tag_provider_key(&tag),
            tx,
        })
        .await?;
        let issuers = rx.await.map_err(|_| CoreError::Shutdown)?;

        let mut hits = Vec::new();
        for issuer in issuers {
            // `fetch_feed` vérifie signature + émetteur et alimente le catalogue.
            let Ok(Some(feed)) = self.fetch_feed(issuer).await else {
                continue;
            };
            for e in &feed.entries {
                if e.tags.iter().any(|t| t == &tag) {
                    if let Ok(cid) = e.cid.parse::<Cid>() {
                        hits.push(crate::catalog::SearchHit {
                            issuer,
                            cid,
                            title: e.title.clone(),
                            tags: e.tags.clone(),
                        });
                    }
                }
            }
        }
        Ok(hits)
    }

    /// Récupère le feed d'un créateur depuis la DHT (découverte hors gossip).
    /// Vérifie la signature et l'émetteur, retient le `seq` le plus élevé, et
    /// l'applique au catalogue local. Renvoie `None` si aucun feed valide
    /// trouvé, **ou** si l'émetteur est une clé bloquée (checkpoint
    /// modération, voir `fetch_feed_inner`).
    pub async fn fetch_feed(&self, issuer: PeerId) -> CoreResult<Option<Feed>> {
        fetch_feed_inner(
            &self.cmd_tx,
            &self.catalog,
            &self.catalog_events,
            &self.subscriptions,
            &self.moderation,
            &self.blocked_channels,
            issuer,
        )
        .await
    }

    /// Résout un aperçu de channel par lien (spec 2026-07-23, partie A) :
    /// **catalogue d'abord** (aucun appel réseau si l'émetteur y figure déjà),
    /// sinon [`Node::fetch_feed`] (DHT) puis relecture du catalogue qu'il vient
    /// d'alimenter. Toujours absent après ça → `CoreError::NoProviders`.
    ///
    /// Un channel **bloqué localement reste résolvable** (aperçu autorisé,
    /// `blocked = true`) : c'est un choix délibéré, l'utilisateur qui a bloqué
    /// doit pouvoir revoir ce qu'il a bloqué (ex. avant un déblocage). Mais le
    /// checkpoint de modération à l'ingestion (lot d, `is_key_blocked_inner`)
    /// rejette les feeds d'une clé bloquée *avant* `Catalog::apply` — donc
    /// `fetch_feed` renvoie systématiquement `None` pour une clé bloquée (voir
    /// `fetch_feed_from_key_blocked_issuer_returns_none`, contrat déjà testé,
    /// à ne pas casser). Pour rester cohérent avec CE rejet tout en honorant
    /// « bloqué reste résolvable », `resolve_channel` interroge la DHT
    /// lui-même via `fetch_verified_feed_from_dht` (même vérification
    /// signature/émetteur que `fetch_feed`, mais SANS le filtre modération) et
    /// construit l'aperçu directement depuis ce `Feed` — sans jamais
    /// l'insérer au catalogue via `Catalog::apply`.
    ///
    /// `subscribed` reflète l'état réel des abonnements ; pour un bloqué il
    /// est toujours faux, le blocage désabonnant (lot d) — aucun abonnement
    /// implicite n'est créé ou supposé ici, l'aperçu est un instantané pur.
    ///
    /// Même nuance pour un émetteur INCONNU mais non bloqué, sur un catalogue
    /// déjà à sa borne anti-DoS (1024, `Catalog::apply`) : l'insertion est
    /// refusée silencieusement par `fetch_feed`, la relecture ne retrouve donc
    /// rien — on construit alors l'aperçu depuis le `Feed` déjà vérifié plutôt
    /// que de supposer la relecture systématiquement gagnante (borne du
    /// catalogue ≠ échec de résolution).
    ///
    /// Discipline verrous : les instantanés (catalogue, abonnements, blocage)
    /// sont pris et relâchés avant tout `.await` — `catalog_entries()`,
    /// `subscriptions_snapshot()` et `is_key_blocked_inner` sont toutes des
    /// fonctions synchrones qui clonent puis relâchent leur verrou en interne.
    pub async fn resolve_channel(&self, issuer: PeerId) -> CoreResult<ChannelPreview> {
        // 1. Catalogue d'abord : aucun réseau si l'entrée est déjà connue.
        if let Some(entry) = self
            .catalog_entries()
            .into_iter()
            .find(|e| e.issuer == issuer)
        {
            let subscribed = self.subscriptions_snapshot().contains(&issuer);
            let blocked = is_key_blocked_inner(&self.moderation, &self.blocked_channels, &issuer);
            return Ok(ChannelPreview {
                issuer,
                channel: entry.channel,
                items: entry.items,
                subscribed,
                blocked,
            });
        }

        // 2. Absent du catalogue. Une clé bloquée n'y entrera jamais (rejet à
        // l'ingestion, lot d) : chemin dédié, sans `Catalog::apply`.
        if is_key_blocked_inner(&self.moderation, &self.blocked_channels, &issuer) {
            let feed = fetch_verified_feed_from_dht(&self.cmd_tx, issuer)
                .await?
                .ok_or_else(|| CoreError::NoProviders(issuer.to_string()))?;
            return Ok(ChannelPreview {
                issuer,
                channel: feed.channel.clone(),
                items: catalog_items_from_feed(&feed),
                subscribed: false, // le blocage désabonne (lot d).
                blocked: true,
            });
        }

        // 3. Clé non bloquée : `fetch_feed` alimente déjà le catalogue en cas
        // de succès — MAIS `Catalog::apply` refuse un émetteur inconnu et non
        // souscrit si le catalogue est déjà à sa borne anti-DoS (1024
        // émetteurs, `fetch_feed_inner` ignore ce refus silencieusement).
        // La borne du catalogue n'est PAS un échec de résolution : plutôt que
        // supposer que la relecture retrouvera forcément l'entrée (ce qui
        // paniquait ici auparavant, un `unwrap`/`expect` réseau atteignable
        // depuis la FFI par un simple lien collé sur un catalogue plein), on
        // retombe sur le `Feed` déjà vérifié pour construire l'aperçu si la
        // relecture ne le trouve pas — même chemin que la clé bloquée
        // ci-dessus, sans jamais réinsérer ni réessayer.
        let feed = self
            .fetch_feed(issuer)
            .await?
            .ok_or_else(|| CoreError::NoProviders(issuer.to_string()))?;
        let subscribed = self.subscriptions_snapshot().contains(&issuer);
        if let Some(entry) = self
            .catalog_entries()
            .into_iter()
            .find(|e| e.issuer == issuer)
        {
            return Ok(ChannelPreview {
                issuer,
                channel: entry.channel,
                items: entry.items,
                subscribed,
                blocked: false,
            });
        }
        Ok(ChannelPreview {
            issuer,
            channel: feed.channel.clone(),
            items: catalog_items_from_feed(&feed),
            subscribed,
            blocked: false,
        })
    }

    /// Ingestion : segmente `input` en HLS via ffmpeg, stocke chaque segment
    /// (CID, checkpoint modération #1 via `add`) et un manifeste, puis renvoie le
    /// CID du manifeste (l'identité du « contenu »).
    pub async fn ingest_file(&self, input: &Path) -> CoreResult<Cid> {
        let work = tempfile::tempdir().map_err(CoreError::Io)?;
        let playlist = ingest::run_ffmpeg_hls(input, work.path(), 4).await?;
        let m3u8 = tokio::fs::read_to_string(&playlist).await?;
        let (target, segs) = ingest::parse_playlist(&m3u8, work.path())?;

        let mut segments = Vec::with_capacity(segs.len());
        let mut segment_cids = Vec::with_capacity(segs.len());
        let mut total_bytes = 0u64;
        for (path, duration) in segs {
            let bytes = tokio::fs::read(&path).await?;
            let cid = self.add(&bytes).await?; // modération #1 + store + provide
            total_bytes += self.blockstore.size_of(&cid).unwrap_or(bytes.len() as u64);
            segment_cids.push(cid.to_string());
            segments.push(HlsSegment {
                cid: cid.to_string(),
                duration,
            });
        }
        let manifest = HlsManifest::new(target, segments);
        let manifest_cid = self.add(manifest.to_json()?.as_bytes()).await?;
        total_bytes += self.blockstore.size_of(&manifest_cid).unwrap_or(0);

        // Contenu propre : retenu au SeedIndex, ÉPINGLÉ d'office (jamais
        // évincé sous quota) — c'est le créateur, il seed toujours ce qu'il
        // publie lui-même (spec channels lot c). Garde `contains_manifest` :
        // `SeedIndex::insert` ne déduplique PAS (elle pousse toujours une
        // nouvelle entrée) — un ré-ingest du même contenu (même manifeste,
        // même CID) ne doit pas compter deux fois ses octets dans le quota.
        // L'épinglage, lui, est idempotent (`BTreeSet::insert`) et refait
        // dans tous les cas.
        {
            let mut idx = self
                .seed_index
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let manifest_cid_str = manifest_cid.to_string();
            if !idx.contains_manifest(&manifest_cid_str) {
                idx.insert(
                    self.peer_id.to_string(),
                    SeededPublication {
                        manifest_cid: manifest_cid_str.clone(),
                        segment_cids,
                        total_bytes,
                        order: 0, // ignoré par `insert`, réassigné par l'index.
                    },
                );
            }
            idx.pin(&manifest_cid_str);
            if let Err(e) = seeding::save_seed_index(&self.blockstore, &idx) {
                tracing::warn!("persistance de l'index de seed échouée: {e}");
            }
        }
        let _ = self.seed_events.send(());
        Ok(manifest_cid)
    }

    /// Reconstruit un HLS jouable depuis un manifeste : récupère le manifeste et
    /// tous ses segments (checkpoint #2 via `get`), les écrit dans `out_dir` et
    /// génère un `index.m3u8`. Renvoie le chemin du playlist.
    pub async fn fetch_hls(&self, manifest_cid: Cid, out_dir: &Path) -> CoreResult<PathBuf> {
        match self.fetch_hls_inner(manifest_cid, out_dir).await {
            Ok(playlist) => Ok(playlist),
            Err(e) => {
                // Pas de sortie partielle : un segment manquant/refusé ne doit
                // pas laisser de `.ts` orphelins sans `index.m3u8` jouable.
                let _ = tokio::fs::remove_dir_all(out_dir).await;
                Err(e)
            }
        }
    }

    async fn fetch_hls_inner(&self, manifest_cid: Cid, out_dir: &Path) -> CoreResult<PathBuf> {
        // Politique : `Seed` si le manifeste appartient à un channel souscrit
        // (suivi actif — on veut le resservir), `Stream` sinon (simple
        // consultation, ex. Explorer) — le retrait de seed-what-you-consume ne
        // doit pas priver un channel suivi de sa réplication. On retient aussi
        // l'émetteur de l'entrée souscrite : c'est l'issuer correct pour
        // enregistrer la publication au SeedIndex ci-dessous (finding M1).
        let subscribed_issuer = self
            .catalog_subscribed()
            .into_iter()
            .find(|e| e.cids.contains(&manifest_cid))
            .map(|e| e.issuer);
        let policy = if subscribed_issuer.is_some() {
            StorePolicy::Seed
        } else {
            StorePolicy::Stream
        };
        let bytes = self.get_with(manifest_cid, policy).await?;
        let manifest = HlsManifest::from_json(&bytes)?;
        tokio::fs::create_dir_all(out_dir).await?;
        for seg in &manifest.segments {
            let cid: Cid = seg.cid.parse().map_err(CoreError::Cid)?;
            let data = self.get_with(cid, policy).await?;
            tokio::fs::write(out_dir.join(format!("{}.ts", seg.cid)), &data).await?;
        }
        let playlist = out_dir.join("index.m3u8");
        tokio::fs::write(&playlist, manifest.to_m3u8()).await?;

        // M1 (revue finale lot c) : `get_with(Seed)` ci-dessus met déjà les
        // blocs en cache et les réannonce, mais ne touchait pas le SeedIndex —
        // la publication restait invisible du quota (`storage_stats`) et
        // survivait à un désabonnement (`unsubscribe` ne purge que ce que le
        // SeedIndex connaît). On l'y enregistre si elle n'y est pas déjà.
        if let Some(issuer) = subscribed_issuer {
            self.register_seeded_publication(issuer, manifest_cid, &manifest);
        }

        Ok(playlist)
    }

    /// Enregistre au SeedIndex une publication déjà récupérée en politique
    /// `Seed` par [`Node::fetch_hls_inner`] (finding M1) : sans cela, le
    /// contenu était mis en cache et réannoncé par `get_with(Seed)` mais
    /// restait absent du SeedIndex — ni compté au quota, ni purgé à un
    /// désabonnement ultérieur. Octets mesurés via `Blockstore::size_of` (les
    /// blocs sont déjà en cache à cet appel), publication NON épinglée
    /// (mêmes règles d'éviction qu'une publication seedée proactivement).
    /// No-op si déjà indexée (ex. seedée entre-temps par la boucle
    /// proactive).
    ///
    /// Compromis assumé : ce chemin n'applique PAS le quota (`make_room_for`)
    /// — une lecture au premier plan (« regarder maintenant ») n'est pas
    /// retenue par la borne à l'insertion, `used` peut donc la dépasser
    /// temporairement. L'éviction de la boucle proactive rétablit la borne au
    /// prochain passage qui aura besoin de place.
    fn register_seeded_publication(
        &self,
        issuer: PeerId,
        manifest_cid: Cid,
        manifest: &HlsManifest,
    ) {
        let manifest_key = manifest_cid.to_string();
        let mut idx = self
            .seed_index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if idx.contains_manifest(&manifest_key) {
            return;
        }
        let segment_cids: Vec<String> = manifest.segments.iter().map(|s| s.cid.clone()).collect();
        let mut total_bytes = self.blockstore.size_of(&manifest_cid).unwrap_or(0);
        for seg in &segment_cids {
            if let Ok(cid) = seg.parse::<Cid>() {
                total_bytes += self.blockstore.size_of(&cid).unwrap_or(0);
            }
        }
        idx.insert(
            issuer.to_string(),
            SeededPublication {
                manifest_cid: manifest_key,
                segment_cids,
                total_bytes,
                order: 0, // ignoré par `insert`, réassigné par l'index.
            },
        );
        if let Err(e) = seeding::save_seed_index(&self.blockstore, &idx) {
            tracing::warn!("persistance de l'index de seed échouée: {e}");
        }
        drop(idx);
        let _ = self.seed_events.send(());
    }

    /// Instantané des entrées du catalogue reconstruit (un feed par émetteur).
    pub fn catalog_entries(&self) -> Vec<CatalogEntry> {
        self.catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries()
    }

    /// Instantané (non trié) des abonnements — usage interne (exemption de
    /// borne du catalogue, cf. `Catalog::apply`).
    fn subscriptions_snapshot(&self) -> HashSet<PeerId> {
        self.subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .copied()
            .collect()
    }

    /// S'abonne à un émetteur (channel) : persiste immédiatement (état privé,
    /// spec channels §2 — jamais publié sur le réseau), puis déclenche un
    /// `fetch_feed` **best-effort** en tâche de fond (ne bloque pas le retour ;
    /// la boucle de suivi périodique prendra le relais si ce coup d'essai
    /// échoue).
    ///
    /// Refuse un émetteur bloqué — **localement** (tâche 3, message "channel
    /// bloqué localement") OU par **denylist** (tâche 2, message "channel
    /// banni par denylist") — via `CoreError::Moderated` (et non
    /// `CoreError::Moderation`, réservée aux erreurs de format/signature des
    /// denylists elles-mêmes) pour que le contrat FFI v8 remonte bien
    /// `FfiError::Moderated` (UX « refus volontaire »), pas `InvalidInput`
    /// (UX « erreur de saisie ») — cf. `From<CoreError> for FfiError`. Ce
    /// refus symétrique ferme un cas d'amplification : sans lui,
    /// `subscribe()` exempterait l'émetteur de la borne du catalogue
    /// (`subscribed` dans `Catalog::apply`) alors même que ses feeds y sont
    /// de toute façon rejetés à l'ingestion (voir `fetch_feed_inner`/
    /// `handle_feed_message`) — un abonné aurait aussi fait tourner
    /// `follow_loop` en pure perte pour cet émetteur indéfiniment.
    pub fn subscribe(&self, issuer: PeerId) -> CoreResult<()> {
        if self
            .blocked_channels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&issuer)
        {
            return Err(CoreError::Moderated("channel bloqué localement".into()));
        }
        if is_key_blocked_inner(&self.moderation, &self.blocked_channels, &issuer) {
            return Err(CoreError::Moderated("channel banni par denylist".into()));
        }
        {
            let mut subs = self
                .subscriptions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            subs.insert(issuer);
            save_subscriptions(&self.blockstore, &subs)?;
        }
        let cmd_tx = self.cmd_tx.clone();
        let catalog = self.catalog.clone();
        let catalog_events = self.catalog_events.clone();
        let subscriptions = self.subscriptions.clone();
        let moderation = self.moderation.clone();
        let blocked_channels = self.blocked_channels.clone();
        tokio::spawn(async move {
            if let Err(e) = fetch_feed_inner(
                &cmd_tx,
                &catalog,
                &catalog_events,
                &subscriptions,
                &moderation,
                &blocked_channels,
                issuer,
            )
            .await
            {
                tracing::debug!("fetch immédiat à l'abonnement échoué pour {issuer}: {e}");
            }
        });
        Ok(())
    }

    /// Se désabonne d'un émetteur : persiste immédiatement, puis purge du
    /// SeedIndex les publications NON épinglées de ce channel (spec channels
    /// lot c) — les blocs correspondants sont supprimés du magasin, SAUF
    /// ceux encore référencés par une autre publication indexée (`all_cids`
    /// de l'index restant). Le feed déjà appliqué au catalogue n'est pas
    /// rétroactivement retiré (le catalogue reste un instantané "meilleur
    /// effort" de ce que le nœud a vu).
    pub fn unsubscribe(&self, issuer: PeerId) -> CoreResult<()> {
        {
            let mut subs = self
                .subscriptions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            subs.remove(&issuer);
            save_subscriptions(&self.blockstore, &subs)?;
        }

        let evicted = {
            let mut idx = self
                .seed_index
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let evicted = idx.purge_issuer(&issuer.to_string(), true);
            if !evicted.is_empty() {
                if let Err(e) = seeding::save_seed_index(&self.blockstore, &idx) {
                    tracing::warn!("persistance de l'index de seed échouée: {e}");
                }
            }
            evicted
        };
        if !evicted.is_empty() {
            remove_unshared_blocks(&self.blockstore, &self.seed_index, &evicted);
            let _ = self.seed_events.send(());
        }
        // Réveille `seed_loop` même si rien n'a été purgé (ex. désabonnement
        // d'un émetteur sans publication indexée) : la place libérée par une
        // purge peut suffire à faire rentrer une publication ailleurs
        // sautée faute de quota, ET `seed_loop` vide `quota_blocked` sur
        // cette branche (cf. son `tokio::select!`) — sans ce réveil, une
        // publication mémorisée bloquée resterait sautée indéfiniment même
        // après que le désabonnement a libéré son quota.
        let _ = self.seed_wake.send(());
        Ok(())
    }

    /// Abonnements courants, triés (stabilité d'affichage).
    pub fn subscriptions(&self) -> Vec<PeerId> {
        self.subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .copied()
            .collect()
    }

    /// Bloque un channel LOCALEMENT (tâche 3) : préférence strictement privée
    /// de ce nœud — invisible partout pour lui, plus jamais retéléchargé.
    /// **Aucun effet réseau** : pas de rapport, pas de record signé, rien
    /// publié (contrairement au blocage par denylist, tâche 2, qui est un
    /// choix fédéré partagé). Persiste immédiatement, désabonne si souscrit,
    /// puis purge catalogue + SeedIndex + blockstore de ce que ce nœud avait
    /// lui-même attribué à cet émetteur — **pins compris**, la modération
    /// locale outrepasse les pins comme la modération par denylist (réutilise
    /// `purge_blocked_issuer`, mécanisme partagé avec la tâche 2). Notifie
    /// `catalog_events` et `seed_events` (UI : le channel doit disparaître
    /// immédiatement des deux vues, seedée ou non).
    pub async fn block_channel(&self, issuer: PeerId) -> CoreResult<()> {
        {
            let mut set = self
                .blocked_channels
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            set.insert(issuer);
            save_blocked_channels(&self.blockstore, &set)?;
        }
        // Le blocage lui-même est déjà durablement persisté ci-dessus : un
        // échec de persistance du retrait d'abonnement (best-effort comme
        // ailleurs dans ce module, ex. `seed_index`/`feed_seq`) ne doit pas
        // faire échouer tout le blocage ni laisser un état mémoire/disque
        // incohérent (finding M1 — ni `?` après une mutation déjà appliquée
        // en mémoire, ni blocage rapporté en échec alors qu'il a réussi).
        {
            let mut subs = self
                .subscriptions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if subs.remove(&issuer) {
                if let Err(e) = save_subscriptions(&self.blockstore, &subs) {
                    tracing::warn!(
                        "persistance du désabonnement (blocage local de {issuer}) échouée: {e}"
                    );
                }
            }
        }
        let entry_cids: Vec<Cid> = self
            .catalog_entries()
            .into_iter()
            .find(|e| e.issuer == issuer)
            .map(|e| e.cids)
            .unwrap_or_default();
        self.purge_blocked_issuer(issuer, &entry_cids).await;
        let _ = self.catalog_events.send(());
        let _ = self.seed_events.send(());
        Ok(())
    }

    /// Débloque un channel bloqué localement : retire la préférence. Rien
    /// n'est retéléchargé automatiquement — le contenu revient naturellement
    /// via gossip/DHT au prochain feed reçu de cet émetteur (comme n'importe
    /// quel émetteur non souscrit et non encore vu).
    pub fn unblock_channel(&self, issuer: PeerId) -> CoreResult<()> {
        let mut set = self
            .blocked_channels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        set.remove(&issuer);
        save_blocked_channels(&self.blockstore, &set)?;
        Ok(())
    }

    /// Channels bloqués localement, triés (stabilité d'affichage — même
    /// patron que `subscriptions`).
    pub fn blocked_channels(&self) -> Vec<PeerId> {
        self.blocked_channels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .copied()
            .collect()
    }

    /// Épingle un manifeste : exempté d'éviction par le seed proactif (lot c),
    /// qu'il soit ou non déjà retenu par `seed_loop`.
    pub fn pin(&self, manifest_cid: Cid) -> CoreResult<()> {
        let mut idx = self
            .seed_index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        idx.pin(&manifest_cid.to_string());
        seeding::save_seed_index(&self.blockstore, &idx)
    }

    /// Retire l'épinglage d'un manifeste (redevient évictable sous quota).
    pub fn unpin(&self, manifest_cid: Cid) -> CoreResult<()> {
        let mut idx = self
            .seed_index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        idx.unpin(&manifest_cid.to_string());
        seeding::save_seed_index(&self.blockstore, &idx)
    }

    /// `(publications_seedées, publications_totales_du_feed_courant)` pour un
    /// émetteur : mesure la couverture du seed proactif sur son feed courant
    /// tel que connu du catalogue local (pas nécessairement souscrit).
    pub fn seed_status(&self, issuer: PeerId) -> (u64, u64) {
        let Some(entry) = self
            .catalog_entries()
            .into_iter()
            .find(|e| e.issuer == issuer)
        else {
            return (0, 0);
        };
        let (seeded, total, _pinned) = self.seed_coverage(&entry.cids);
        (seeded, total)
    }

    /// `(publications_seedées, publications_totales, pins)` pour une liste de
    /// CIDs **déjà résolue** (ex. `entry.cids` d'une entrée de catalogue en
    /// main) — un seul verrou `seed_index`, sans re-parcourir le catalogue.
    /// `seed_status`/`pinned_manifests_of` s'appuient dessus pour un appel
    /// isolé par PeerId ; la FFI (`catalog_entry_to_ffi`) l'appelle
    /// directement par entrée pour rester O(N) sur tout le catalogue au lieu
    /// de re-cloner le catalogue entier (verrou + reconstruction CRDT) par
    /// entrée, ce qui serait O(N²) (retour de review, tâche c5).
    pub fn seed_coverage(&self, cids: &[Cid]) -> (u64, u64, Vec<Cid>) {
        let idx = self
            .seed_index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut seeded = 0u64;
        let mut pinned = Vec::new();
        for c in cids {
            let s = c.to_string();
            if idx.contains_manifest(&s) {
                seeded += 1;
            }
            if idx.is_pinned(&s) {
                pinned.push(*c);
            }
        }
        (seeded, cids.len() as u64, pinned)
    }

    /// `(octets_utilisés, quota_octets)` — `used` vient du SeedIndex (source
    /// de vérité pour la comptabilité de quota, cf. rapport tâche c4 §Minor),
    /// pas d'un balayage concurrent du blockstore.
    pub fn storage_stats(&self) -> (u64, u64) {
        let used = self
            .seed_index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .total_bytes();
        let quota = *self
            .seed_quota
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (used, quota)
    }

    /// Définit le quota de seed : persiste puis réveille `seed_loop` (une
    /// baisse de quota peut nécessiter une éviction immédiate, une hausse
    /// peut permettre de reprendre des publications sautées faute de place).
    pub fn set_seed_quota(&self, bytes: u64) -> CoreResult<()> {
        {
            let mut quota = self
                .seed_quota
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *quota = bytes;
        }
        seeding::save_seed_quota(&self.blockstore, bytes)?;
        let _ = self.seed_wake.send(());
        Ok(())
    }

    /// CIDs de manifestes de `issuer` (tels que connus du catalogue local)
    /// actuellement épinglés. Sert le contrat FFI v7 (`FfiCatalogEntry.pinned`)
    /// pour un appel isolé par PeerId ; la FFI, elle, appelle `seed_coverage`
    /// directement par entrée (voir sa doc) pour rester O(N) sur le catalogue.
    pub fn pinned_manifests_of(&self, issuer: PeerId) -> Vec<Cid> {
        let Some(entry) = self
            .catalog_entries()
            .into_iter()
            .find(|e| e.issuer == issuer)
        else {
            return Vec::new();
        };
        let (_seeded, _total, pinned) = self.seed_coverage(&entry.cids);
        pinned
    }

    /// Entrées du catalogue restreintes aux émetteurs souscrits.
    pub fn catalog_subscribed(&self) -> Vec<CatalogEntry> {
        let subs = self.subscriptions_snapshot();
        self.catalog_entries()
            .into_iter()
            .filter(|e| subs.contains(&e.issuer))
            .collect()
    }

    /// Injecte un feed tiers directement dans le catalogue local, hors réseau
    /// (tests uniquement : simule la réception d'un feed sans dépendre du
    /// gossip ou de la DHT).
    #[doc(hidden)]
    pub fn apply_feed_for_tests(&self, feed: Feed) -> CoreResult<bool> {
        let subs = self.subscriptions_snapshot();
        let changed = self
            .catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .apply(feed, &subs)?;
        if changed {
            let _ = self.catalog_events.send(());
        }
        Ok(changed)
    }

    /// Profil de channel courant (instantané).
    pub fn channel_profile(&self) -> ChannelMeta {
        self.channel_profile
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Définit le profil de channel : valide les bornes, persiste, puis
    /// republie le feed courant s'il existe (le changement de nom se propage
    /// sans attendre la prochaine publication).
    pub async fn set_channel_profile(&self, meta: ChannelMeta) -> CoreResult<()> {
        if meta.name.len() > crate::feed::MAX_CHANNEL_NAME_LEN
            || meta.description.len() > crate::feed::MAX_CHANNEL_DESC_LEN
        {
            return Err(CoreError::Network("profil de channel hors bornes".into()));
        }
        if let Some(avatar) = &meta.avatar_cid {
            avatar
                .parse::<Cid>()
                .map_err(|_| CoreError::Network("avatar_cid invalide".into()))?;
        }
        let json = serde_json::to_string(&meta)
            .map_err(|e| CoreError::Network(format!("json profil: {e}")))?;
        std::fs::write(channel_profile_path(&self.blockstore), json)?;
        *self
            .channel_profile
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = meta;

        // Republie le feed courant (mêmes entries) pour propager le profil.
        let own = self
            .catalog_entries()
            .into_iter()
            .find(|e| e.issuer == self.peer_id());
        if let Some(entry) = own {
            let entries: Vec<FeedEntry> = entry
                .items
                .iter()
                .map(|i| FeedEntry {
                    cid: i.cid.to_string(),
                    title: i.title.clone(),
                    tags: i.tags.clone(),
                })
                .collect();
            self.publish_feed_with(&entries).await?;
        }
        Ok(())
    }

    /// Tous les CIDs connus du catalogue.
    pub fn catalog_cids(&self) -> HashSet<Cid> {
        self.catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .all_cids()
    }

    /// Stocke un bloc localement et l'annonce dans la DHT (provider record).
    ///
    /// CHECKPOINT MODÉRATION #1 (ingestion) : un contenu matché est refusé — ni
    /// stocké, ni annoncé.
    pub async fn add(&self, bytes: &[u8]) -> CoreResult<Cid> {
        let cid = cid_for(bytes);
        if self.is_blocked(&cid) {
            return Err(CoreError::Moderated(cid.to_string()));
        }
        let cid = self.blockstore.put(bytes)?;
        self.provide(cid).await?;
        Ok(cid)
    }

    /// Réannonce dans la DHT TOUS les CIDs détenus localement (provider records).
    /// Indispensable au démarrage d'un seeder : le store de providers Kademlia est
    /// volatile, donc après un redémarrage les blocs détenus ne sont plus annoncés
    /// tant qu'on ne les republie pas. Renvoie le nombre de CIDs réannoncés.
    pub async fn reprovide_all(&self) -> CoreResult<usize> {
        let cids = self.blockstore.list()?;
        for cid in &cids {
            self.provide(*cid).await?;
        }
        Ok(cids.len())
    }

    /// Réannonce dans la DHT les feeds SIGNÉS que ce nœud détient
    /// légitimement : le sien propre (s'il a déjà publié — il figure alors
    /// dans son propre catalogue, voir `publish_feed_with`) et ceux des
    /// créateurs **souscrits** présents dans le catalogue. Contrairement à
    /// [`Node::reprovide_all`] (provider records de blocs), ceci re-PUT le
    /// RECORD DE FEED lui-même (`/champinium/feed/<peerid>`) — sans quoi il
    /// s'éteint au TTL du `MemoryStore` Kademlia dès que son émetteur est
    /// hors ligne, y compris si celui-ci a bel et bien publié (voir ADR
    /// 0007). Le nœud ne fait que réémettre les octets déjà signés par
    /// l'émetteur d'origine — sa propre clé n'est jamais engagée pour un
    /// tiers, la signature republiée reste celle du créateur.
    ///
    /// Ne republie QUE les abonnements (jamais tout le catalogue) :
    /// cohérent avec « les abonnés SONT l'infrastructure du créateur » (seuls
    /// ceux qui ont choisi de suivre un émetteur portent la durabilité de son
    /// feed) et borne l'amplification réseau (un nœud avec un catalogue
    /// borné à 1024 émetteurs mais 3 abonnements ne republie que 3 feeds, pas
    /// 1024). Un émetteur bloqué (denylist souscrite ou blocage local de
    /// channel, `is_key_blocked_inner`) est exclu — republier le feed d'une
    /// clé qu'on refuse par ailleurs serait incohérent avec la modération.
    ///
    /// Best-effort (comme `publish_feed_with` : le PUT n'est pas attendu,
    /// juste envoyé à la boucle réseau) ; renvoie le nombre de feeds pour
    /// lesquels un PUT a été émis.
    pub async fn republish_known_feeds(&self) -> CoreResult<usize> {
        let subs = self.subscriptions_snapshot();
        let to_republish: Vec<(PeerId, Vec<u8>)> = {
            let catalog = self
                .catalog
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            catalog
                .issuers()
                .filter(|issuer| **issuer == self.peer_id || subs.contains(*issuer))
                .filter(|issuer| {
                    !is_key_blocked_inner(&self.moderation, &self.blocked_channels, issuer)
                })
                .filter_map(|issuer| {
                    let data = catalog.feed_for(issuer)?.to_json().ok()?.into_bytes();
                    Some((*issuer, data))
                })
                .collect()
        };

        let mut count = 0usize;
        for (issuer, data) in to_republish {
            let (tx, _rx) = oneshot::channel();
            let _ = self
                .cmd_tx
                .send(Command::PutRecord {
                    key: feed_record_key(&issuer),
                    value: data,
                    tx,
                })
                .await;
            count += 1;
        }
        Ok(count)
    }

    /// Annonce que ce nœud fournit `cid`. **Ne fait rien** si le CID est bloqué :
    /// s'annoncer fournisseur d'un contenu qu'on refuse de servir serait à la
    /// fois incohérent et un signal au réseau qu'on le détient.
    pub async fn provide(&self, cid: Cid) -> CoreResult<()> {
        provide_inner(&self.cmd_tx, &self.moderation, cid).await
    }

    /// Recherche les fournisseurs d'un CID via la DHT.
    pub async fn get_providers(&self, cid: Cid) -> CoreResult<HashSet<PeerId>> {
        get_providers_inner(&self.cmd_tx, cid).await
    }

    /// Facteur de réplication **mesuré** d'un CID : nombre de fournisseurs
    /// annoncés dans la DHT (soi-même compris). Observabilité de la mitigation
    /// du risque #1 (persistance) : un `get_with(Seed)` doit le faire croître,
    /// un `get` (Stream) par défaut ne le doit plus.
    pub async fn replication_factor(&self, cid: Cid) -> CoreResult<usize> {
        Ok(self.get_providers(cid).await?.len())
    }

    /// Récupère un bloc SANS engager le nœud à le seeder : alias de
    /// [`Node::get_with`] avec [`StorePolicy::Stream`]. C'est le nouveau
    /// défaut depuis le retrait de seed-what-you-consume — un hit local est
    /// toujours servi, mais une récupération réseau n'est ni mise en cache ni
    /// annoncée. Pour l'ancien comportement (stocker + fournir), utiliser
    /// `get_with(cid, StorePolicy::Seed)`.
    pub async fn get(&self, cid: Cid) -> CoreResult<Vec<u8>> {
        self.get_with(cid, StorePolicy::Stream).await
    }

    /// Récupère un bloc : cache local → sinon découverte DHT + transfert + vérif.
    /// Interroge **tous les fournisseurs en parallèle** et retient la première
    /// réponse valide. `policy` gouverne le sort d'un bloc nouvellement récupéré
    /// depuis le réseau : `Seed` le met en cache et le réannonce (le nœud
    /// devient fournisseur), `Stream` se contente de rendre les octets.
    ///
    /// Exception : si le bloc était présent localement mais **corrompu**, la
    /// réparation stocke et réannonce toujours, indépendamment de `policy` — le
    /// nœud le détenait déjà légitimement (par `add` ou un `get_with(Seed)`
    /// antérieur), la réparation restaure cet état plutôt que d'en décider un
    /// nouveau.
    ///
    /// CHECKPOINT MODÉRATION #2 (réception) : un contenu matché n'est ni récupéré,
    /// ni mis en cache, ni fourni — quelle que soit `policy` — et le refus est
    /// **signalé** aux pairs (rapport signé sur le topic des signalements,
    /// best-effort).
    pub(crate) async fn get_with(&self, cid: Cid, policy: StorePolicy) -> CoreResult<Vec<u8>> {
        let result = get_with_inner(
            &self.blockstore,
            &self.moderation,
            &self.cmd_tx,
            self.peer_id,
            &self.keypair,
            &self.reports,
            cid,
            policy,
        )
        .await;

        // Repli de récupération froide (ADR 0008, CS-a tâche 3) : uniquement
        // sur `NoProviders` (plus aucun fournisseur P2P), jamais sur les
        // autres erreurs (`Moderated` en particulier reste un refus ferme).
        // No-op garanti si la feature est absente ou si aucun `ColdStore`
        // n'est câblé : `result` est alors renvoyé tel quel, comportement
        // identique à avant cette tâche.
        #[cfg(feature = "cold-storage")]
        if matches!(&result, Err(CoreError::NoProviders(_)))
            && self.cold_retrieval_enabled.load(Ordering::Relaxed)
        {
            if let Some(cold) = self.cold.as_ref() {
                return cold_fallback_inner(
                    &self.blockstore,
                    &self.moderation,
                    &self.cmd_tx,
                    &self.reports,
                    &self.keypair,
                    cold,
                    cid,
                    policy,
                )
                .await;
            }
        }

        result
    }

    /// Active/désactive le repli de récupération froide et persiste le choix
    /// (dotfile `.cold_enabled`, à côté des blocs). Débrayable sans
    /// recompiler : coût réseau/monétaire du froid, ou préférence de vie
    /// privée de l'utilisateur.
    #[cfg(feature = "cold-storage")]
    pub fn set_cold_retrieval(&self, enabled: bool) -> CoreResult<()> {
        self.cold_retrieval_enabled
            .store(enabled, Ordering::Relaxed);
        save_cold_enabled(&self.blockstore, enabled)
    }

    /// État courant du débrayage de repli froid (vrai par défaut).
    #[cfg(feature = "cold-storage")]
    pub fn cold_retrieval_enabled(&self) -> bool {
        self.cold_retrieval_enabled.load(Ordering::Relaxed)
    }

    /// Injecte un backend [`ColdStore`] (tests/configuration) : le câblage
    /// d'un backend réel (`ArweaveColdStore`) est un détail de configuration
    /// laissé à un suivi ultérieur — cette tâche prouve le repli avec un
    /// backend injectable.
    #[cfg(feature = "cold-storage")]
    #[doc(hidden)]
    pub fn with_cold_for_tests(&self, cold: Arc<dyn ColdStore>) -> Self {
        let mut node = self.clone();
        node.cold = Some(cold);
        node
    }

    /// Passerelle de test vers `get_with` : `StorePolicy` est `pub(crate)`
    /// (interne au crate) et les tests d'intégration du repli froid (tâche 3)
    /// vivent hors crate — ce point d'entrée `#[doc(hidden)]` expose le seul
    /// comportement observable nécessaire (`seed: bool`) sans élargir l'API
    /// publique pour de vrai.
    #[cfg(feature = "cold-storage")]
    #[doc(hidden)]
    pub async fn get_with_policy_for_tests(&self, cid: Cid, seed: bool) -> CoreResult<Vec<u8>> {
        let policy = if seed {
            StorePolicy::Seed
        } else {
            StorePolicy::Stream
        };
        self.get_with(cid, policy).await
    }

    /// Injecte un backend [`ColdStore`] **et** un portefeuille Arweave
    /// (tests/configuration) : comme `with_cold_for_tests`, le câblage de
    /// production (config utilisateur) est un suivi ultérieur (lot CS-b) — cette
    /// tâche prouve le devis + l'archivage avec des dépendances injectées.
    #[cfg(feature = "cold-storage")]
    #[doc(hidden)]
    pub fn with_cold_and_wallet_for_tests(
        &self,
        cold: Arc<dyn ColdStore>,
        wallet: crate::coldstore::ArweaveWallet,
    ) -> Self {
        let mut node = self.clone();
        node.cold = Some(cold);
        node.wallet = Some(wallet);
        node
    }

    /// Rassemble les octets d'une publication (manifeste + tous ses segments)
    /// **depuis le blockstore local** — l'archivage ne concerne que le propre
    /// contenu du créateur, épinglé chez lui (aucun appel réseau). Renvoie un
    /// [`ArchivePayload`] dont `items` = `[(manifeste, octets), (segment_i,
    /// octets)…]`. Un bloc absent localement → `BlockNotFound` : on n'archive
    /// pas ce qu'on ne détient pas.
    #[cfg(feature = "cold-storage")]
    fn gather_publication(&self, manifest_cid: Cid) -> CoreResult<ArchivePayload> {
        let manifest_bytes = self.blockstore.get(&manifest_cid)?;
        let manifest = HlsManifest::from_json(&manifest_bytes)?;
        let mut items: Vec<(Cid, Vec<u8>)> = Vec::with_capacity(1 + manifest.segments.len());
        items.push((manifest_cid, manifest_bytes));
        for seg in &manifest.segments {
            let cid: Cid = seg.cid.parse().map_err(CoreError::Cid)?;
            let bytes = self.blockstore.get(&cid)?;
            items.push((cid, bytes));
        }
        Ok(ArchivePayload {
            manifest_cid,
            items,
        })
    }

    /// **Premier temps** de l'archivage opt-in (ADR 0008) : rassemble la
    /// publication (manifeste + segments, localement), calcule sa taille et un
    /// **devis** — coût = somme des prix Arweave par item (une transaction
    /// format 2 par bloc), solde courant du portefeuille — et le renvoie **sans
    /// rien envoyer**. `sufficient` = le solde couvre le coût. Aucun AR n'est
    /// dépensé ici. Voir [`Node::confirm_archive`] pour le second temps.
    #[cfg(feature = "cold-storage")]
    pub async fn archive_publication(&self, manifest_cid: Cid) -> CoreResult<ArchiveQuote> {
        let (cold, wallet) = self.cold_and_wallet()?;
        let payload = self.gather_publication(manifest_cid)?;

        let mut bytes: u64 = 0;
        let mut cost_winston: u64 = 0;
        for (_cid, item) in &payload.items {
            bytes += item.len() as u64;
            let price = cold.price(item.len() as u64).await?;
            cost_winston = cost_winston.saturating_add(price);
        }
        let balance_winston = cold.balance(wallet).await?;

        Ok(ArchiveQuote {
            manifest_cid: manifest_cid.to_string(),
            bytes,
            cost_winston,
            balance_winston,
            sufficient: balance_winston >= cost_winston,
        })
    }

    /// **Second temps** de l'archivage opt-in : n'exécute l'upload que sur un
    /// devis au **solde suffisant** (`quote.sufficient`) — sinon `Err` sans
    /// **aucun** envoi. Sur solde suffisant : re-rassemble la publication,
    /// signe et POSTe une transaction par item, **persiste le reçu** (dotfile
    /// `.archives`) et le renvoie. Refus explicite si le solde est insuffisant.
    #[cfg(feature = "cold-storage")]
    pub async fn confirm_archive(&self, quote: &ArchiveQuote) -> CoreResult<ArchiveReceipt> {
        if !quote.sufficient {
            return Err(CoreError::Network(format!(
                "archivage refusé: solde insuffisant ({} < {} winston)",
                quote.balance_winston, quote.cost_winston
            )));
        }
        let (cold, wallet) = self.cold_and_wallet()?;
        let manifest_cid: Cid = quote.manifest_cid.parse().map_err(CoreError::Cid)?;
        let payload = self.gather_publication(manifest_cid)?;

        let receipt = cold.archive(&payload, wallet).await?;
        crate::coldstore::receipts::save_receipt(self.blockstore.root(), &receipt)?;
        Ok(receipt)
    }

    /// Accès conjoint au backend froid et au portefeuille, tous deux requis
    /// pour l'archivage. Erreur claire si l'un manque (aucun câblage de
    /// production dans cette tâche — cf. `with_cold_and_wallet_for_tests`).
    #[cfg(feature = "cold-storage")]
    fn cold_and_wallet(
        &self,
    ) -> CoreResult<(&Arc<dyn ColdStore>, &crate::coldstore::ArweaveWallet)> {
        let cold = self.cold.as_ref().ok_or_else(|| {
            CoreError::Network("archivage: aucun backend de stockage froid configuré".to_string())
        })?;
        let wallet = self.wallet.as_ref().ok_or_else(|| {
            CoreError::Identity("archivage: aucun portefeuille Arweave configuré".to_string())
        })?;
        Ok((cold, wallet))
    }

    /// Demande un bloc précis à un pair précis. Les octets reçus sont **vérifiés
    /// contre le CID** avant d'être renvoyés : un pair ne peut pas faire passer un
    /// contenu arbitraire (tout appelant, pas seulement `get`, est protégé).
    pub async fn request_block(&self, peer: PeerId, cid: Cid) -> CoreResult<Vec<u8>> {
        request_block_inner(&self.cmd_tx, peer, cid).await
    }

    async fn send(&self, cmd: Command) -> CoreResult<()> {
        self.cmd_tx.send(cmd).await.map_err(|_| CoreError::Shutdown)
    }
}

/// Chemin du compteur de `seq` de feed persisté (à côté des blocs ; ignoré par
/// `Blockstore::list` car ce n'est pas un nom de CID valide).
fn feed_seq_path(blockstore: &Blockstore) -> PathBuf {
    blockstore.root().join(".feed_seq")
}

/// Charge le dernier `seq` de feed persisté (0 si absent/illisible).
fn load_feed_seq(blockstore: &Blockstore) -> u64 {
    std::fs::read_to_string(feed_seq_path(blockstore))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Chemin du profil de channel persisté (identité éditoriale, spec channels §1).
fn channel_profile_path(blockstore: &Blockstore) -> PathBuf {
    blockstore.root().join(".channel_profile")
}

/// Charge le profil persisté (défaut si absent/illisible — un profil corrompu
/// ne doit pas empêcher le nœud de démarrer).
fn load_channel_profile(blockstore: &Blockstore) -> ChannelMeta {
    std::fs::read_to_string(channel_profile_path(blockstore))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Chemin du store d'abonnements persisté (état PRIVÉ du nœud — jamais publié
/// sur le réseau, spec channels §2).
fn subscriptions_path(blockstore: &Blockstore) -> PathBuf {
    blockstore.root().join(".subscriptions")
}

/// Charge les abonnements persistés (vide si absent/illisible — un fichier
/// corrompu ne doit pas empêcher le démarrage). Les entrées individuellement
/// invalides (PeerId mal formé) sont ignorées plutôt que de faire échouer tout
/// le chargement.
fn load_subscriptions(blockstore: &Blockstore) -> BTreeSet<PeerId> {
    std::fs::read_to_string(subscriptions_path(blockstore))
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .map(|ids| {
            ids.into_iter()
                .filter_map(|s| s.parse::<PeerId>().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Persiste les abonnements (JSON `Vec<String>` trié — `BTreeSet` le garantit).
fn save_subscriptions(blockstore: &Blockstore, subs: &BTreeSet<PeerId>) -> CoreResult<()> {
    let ids: Vec<String> = subs.iter().map(PeerId::to_string).collect();
    let json = serde_json::to_string(&ids)
        .map_err(|e| CoreError::Network(format!("json abonnements: {e}")))?;
    std::fs::write(subscriptions_path(blockstore), json)?;
    Ok(())
}

/// Chemin du débrayage de repli froid persisté (CS-a tâche 3), à côté des
/// blocs — même patron que `.subscriptions`/`.seed_quota`.
#[cfg(feature = "cold-storage")]
fn cold_enabled_path(blockstore: &Blockstore) -> PathBuf {
    blockstore.root().join(".cold_enabled")
}

/// Charge le débrayage persisté (activé par défaut si absent/illisible — un
/// dotfile corrompu ne doit pas empêcher le démarrage, ni couper silencieusement
/// un repli que l'utilisateur attend).
#[cfg(feature = "cold-storage")]
fn load_cold_enabled(blockstore: &Blockstore) -> bool {
    std::fs::read_to_string(cold_enabled_path(blockstore))
        .ok()
        .and_then(|s| s.trim().parse::<bool>().ok())
        .unwrap_or(true)
}

/// Persiste le débrayage de repli froid.
#[cfg(feature = "cold-storage")]
fn save_cold_enabled(blockstore: &Blockstore, enabled: bool) -> CoreResult<()> {
    std::fs::write(cold_enabled_path(blockstore), enabled.to_string())?;
    Ok(())
}

/// Chemin du store de channels bloqués localement persisté (tâche 3, état
/// PRIVÉ du nœud — jamais publié sur le réseau, même patron que
/// `.subscriptions`).
fn blocked_channels_path(blockstore: &Blockstore) -> PathBuf {
    blockstore.root().join(".blocked_channels")
}

/// Charge les channels bloqués localement (vide si absent/illisible — un
/// fichier corrompu ne doit pas empêcher le démarrage ; entrées
/// individuellement invalides ignorées, même tolérance que
/// `load_subscriptions`).
fn load_blocked_channels(blockstore: &Blockstore) -> BTreeSet<PeerId> {
    std::fs::read_to_string(blocked_channels_path(blockstore))
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .map(|ids| {
            ids.into_iter()
                .filter_map(|s| s.parse::<PeerId>().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Persiste les channels bloqués localement (JSON `Vec<String>` trié —
/// `BTreeSet` le garantit).
fn save_blocked_channels(blockstore: &Blockstore, blocked: &BTreeSet<PeerId>) -> CoreResult<()> {
    let ids: Vec<String> = blocked.iter().map(PeerId::to_string).collect();
    let json = serde_json::to_string(&ids)
        .map_err(|e| CoreError::Network(format!("json channels bloqués: {e}")))?;
    std::fs::write(blocked_channels_path(blockstore), json)?;
    Ok(())
}

/// Interroge la DHT pour le record de feed de `issuer`, ne retient que le
/// candidat de plus grand `seq` dont la signature ET l'émetteur sont
/// vérifiés. Ne fait AUCUNE vérification de modération — c'est la
/// responsabilité de l'appelant (voir `fetch_feed_inner`, qui l'applique
/// juste après, et `Node::resolve_channel`, qui a besoin du feed d'une clé
/// bloquée sans jamais l'insérer au catalogue).
async fn fetch_verified_feed_from_dht(
    cmd_tx: &mpsc::Sender<Command>,
    issuer: PeerId,
) -> CoreResult<Option<Feed>> {
    let (tx, rx) = oneshot::channel();
    cmd_tx
        .send(Command::GetRecord {
            key: feed_record_key(&issuer),
            tx,
        })
        .await
        .map_err(|_| CoreError::Shutdown)?;
    let values = rx.await.map_err(|_| CoreError::Shutdown)?;

    let mut best: Option<Feed> = None;
    for value in values {
        let Ok(feed) = Feed::from_json(&value) else {
            continue;
        };
        if feed.verify().is_err() || feed.issuer_peer_id().ok() != Some(issuer) {
            continue;
        }
        if best.as_ref().is_none_or(|b| feed.seq > b.seq) {
            best = Some(feed);
        }
    }
    Ok(best)
}

/// Cœur de `Node::fetch_feed`, factorisé pour être réutilisable par la boucle
/// de suivi périodique (qui ne détient qu'un clone des champs nécessaires, pas
/// un `Node` entier — voir `follow_loop`). Récupère le feed d'un créateur
/// depuis la DHT (via `fetch_verified_feed_from_dht`, signature + émetteur
/// déjà vérifiés), retient le `seq` le plus élevé, l'applique au catalogue
/// (avec exemption de borne s'il est souscrit).
///
/// CHECKPOINT MODÉRATION (ingestion catalogue, finding M2) : si `issuer` est
/// une clé bloquée, le feed trouvé (signature déjà vérifiée) n'est PAS
/// appliqué au catalogue, et la fonction renvoie `Ok(None)` (« aucun feed
/// exploitable », même sémantique que si rien n'avait été trouvé) plutôt que
/// le feed rejeté : un appelant ne doit pas pouvoir distinguer « rien trouvé »
/// de « trouvé mais bloqué », ce qui éviterait toute fuite d'information sur
/// le contenu d'une clé bannie. `Node::resolve_channel` a besoin de ce feed
/// malgré tout (aperçu autorisé pour un bloqué) : il appelle directement
/// `fetch_verified_feed_from_dht`, plus bas niveau, plutôt que cette fonction.
///
/// N'alimente PLUS de registre de CIDs dérivé depuis le contenu du feed
/// (revue post-implémentation, finding critique I2, retiré après la tâche
/// 3) : capturer les CIDs *listés par* une clé bannie et les bloquer
/// aveuglément permettait à cette clé de faire disparaître le contenu d'un
/// tiers innocent en le mentionnant dans son propre feed — lister un CID ne
/// prouve rien sur qui le détient. L'enforcement se limite désormais à ce
/// que ce nœud peut réellement ATTRIBUER à la clé bannie (son entrée de
/// catalogue, son SeedIndex) — voir `purge_blocked_issuer`.
#[allow(clippy::too_many_arguments)]
async fn fetch_feed_inner(
    cmd_tx: &mpsc::Sender<Command>,
    catalog: &Arc<Mutex<Catalog>>,
    catalog_events: &tokio::sync::broadcast::Sender<()>,
    subscriptions: &Arc<Mutex<BTreeSet<PeerId>>>,
    moderation: &Arc<RwLock<Moderation>>,
    blocked_channels: &Arc<Mutex<BTreeSet<PeerId>>>,
    issuer: PeerId,
) -> CoreResult<Option<Feed>> {
    let Some(feed) = fetch_verified_feed_from_dht(cmd_tx, issuer).await? else {
        return Ok(None);
    };
    if is_key_blocked_inner(moderation, blocked_channels, &issuer) {
        return Ok(None);
    }
    let subs: HashSet<PeerId> = subscriptions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .copied()
        .collect();
    let changed = catalog
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .apply(feed.clone(), &subs);
    if changed.unwrap_or(false) {
        let _ = catalog_events.send(());
    }
    Ok(Some(feed))
}

/// Boucle de suivi actif des channels souscrits (spec channels §2) : à chaque
/// itération, tente un `fetch_feed` pour chaque abonné courant (best-effort,
/// un échec individuel n'interrompt pas la passe), puis dort `follow_interval`
/// avant de recommencer. La toute première itération a lieu **avant** la
/// première attente : elle couvre les abonnements persistés au démarrage
/// (spec : un abonné retrouve ce qui a été publié pendant qu'il était hors
/// ligne).
///
/// `follow_interval` est fixé une fois pour toutes à l'appel (constante de
/// production [`FOLLOW_INTERVAL`], ou surchargée en test via
/// [`Node::with_moderation_and_follow_interval`]) — **pas** un `Arc<Mutex<_>>`
/// mutable après coup : voir le commentaire sur `FOLLOW_INTERVAL` pour la
/// course qu'un tel setter causerait avec cette toute première itération.
///
/// Ne détient qu'un [`Weak`] marqueur de vivacité : dès que toutes les
/// poignées `Node` sont tombées (donc `alive.upgrade()` échoue), la boucle
/// s'arrête et relâche son clone de `cmd_tx` — sinon la boucle d'évènements ne
/// s'arrêterait jamais (`cmd_rx.recv()` attend que tous les émetteurs tombent).
#[allow(clippy::too_many_arguments)]
async fn follow_loop(
    alive: Weak<()>,
    cmd_tx: mpsc::Sender<Command>,
    catalog: Arc<Mutex<Catalog>>,
    catalog_events: tokio::sync::broadcast::Sender<()>,
    subscriptions: Arc<Mutex<BTreeSet<PeerId>>>,
    moderation: Arc<RwLock<Moderation>>,
    blocked_channels: Arc<Mutex<BTreeSet<PeerId>>>,
    follow_interval: Duration,
) {
    loop {
        if alive.upgrade().is_none() {
            return;
        }
        let issuers: Vec<PeerId> = subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .copied()
            .collect();
        for issuer in issuers {
            if let Err(e) = fetch_feed_inner(
                &cmd_tx,
                &catalog,
                &catalog_events,
                &subscriptions,
                &moderation,
                &blocked_channels,
                issuer,
            )
            .await
            {
                tracing::debug!("suivi périodique de {issuer} échoué: {e}");
            }
        }
        tokio::time::sleep(follow_interval).await;
    }
}

// --- Primitives réseau « libres » (paramétrées sur des `Arc` isolés plutôt
// que sur `self: &Node`) — permettent à `seed_loop` de récupérer des blocs
// exactement comme `Node::get_with` sans détenir un `Node` fort (ce qui
// empêcherait la boucle d'évènements de s'arrêter, cf. le commentaire sur
// `alive`). `Node::get_with`/`get_providers`/`provide`/`request_block`
// délèguent à ces mêmes fonctions : aucune logique réseau dupliquée.

fn is_blocked_inner(moderation: &Arc<RwLock<Moderation>>, cid: &Cid) -> bool {
    moderation
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_blocked(cid)
}

/// Vrai si `issuer` est bloqué : par denylist souscrite (`is_blocked_key`,
/// tâche 2) OU par blocage LOCAL de channel (`blocked_channels`, tâche 3) —
/// ensembles fusionnés, même enforcement aux trois checkpoints. Utilisé aux
/// points d'entrée du catalogue (gossip, DHT) pour rejeter les feeds d'une
/// clé bloquée AVANT `Catalog::apply` — voir la doc de `handle_feed_message`
/// pour la justification de ce choix (call site plutôt qu'un paramètre de
/// `apply`).
fn is_key_blocked_inner(
    moderation: &Arc<RwLock<Moderation>>,
    blocked_channels: &Arc<Mutex<BTreeSet<PeerId>>>,
    issuer: &PeerId,
) -> bool {
    moderation
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_blocked_key(issuer)
        || blocked_channels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(issuer)
}

async fn get_providers_inner(
    cmd_tx: &mpsc::Sender<Command>,
    cid: Cid,
) -> CoreResult<HashSet<PeerId>> {
    let (tx, rx) = oneshot::channel();
    cmd_tx
        .send(Command::GetProviders {
            key: RecordKey::new(&cid.to_bytes()),
            tx,
        })
        .await
        .map_err(|_| CoreError::Shutdown)?;
    rx.await.map_err(|_| CoreError::Shutdown)
}

async fn provide_inner(
    cmd_tx: &mpsc::Sender<Command>,
    moderation: &Arc<RwLock<Moderation>>,
    cid: Cid,
) -> CoreResult<()> {
    if is_blocked_inner(moderation, &cid) {
        return Ok(());
    }
    let (tx, rx) = oneshot::channel();
    cmd_tx
        .send(Command::Provide {
            key: RecordKey::new(&cid.to_bytes()),
            tx,
        })
        .await
        .map_err(|_| CoreError::Shutdown)?;
    rx.await.map_err(|_| CoreError::Shutdown)?
}

async fn request_block_inner(
    cmd_tx: &mpsc::Sender<Command>,
    peer: PeerId,
    cid: Cid,
) -> CoreResult<Vec<u8>> {
    let (tx, rx) = oneshot::channel();
    cmd_tx
        .send(Command::RequestBlock { peer, cid, tx })
        .await
        .map_err(|_| CoreError::Shutdown)?;
    let bytes = rx.await.map_err(|_| CoreError::Shutdown)??;
    if !verify(&cid, &bytes) {
        return Err(CoreError::IntegrityMismatch);
    }
    Ok(bytes)
}

async fn emit_report_inner(
    cmd_tx: &mpsc::Sender<Command>,
    reports: &Arc<Mutex<ReportBook>>,
    keypair: &Keypair,
    cid: &Cid,
    reason: &str,
) {
    let Ok(report) = Report::build_signed(keypair, cid, reason) else {
        return;
    };
    {
        let mut book = reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = book.apply(&report);
    }
    let Ok(json) = report.to_json() else { return };
    let (tx, _rx) = oneshot::channel();
    let _ = cmd_tx
        .send(Command::PublishReport {
            data: json.into_bytes(),
            tx,
        })
        .await;
}

/// Cœur de `Node::get_with` (voir sa documentation) — factorisé pour être
/// appelable depuis `seed_loop` sans détenir un `Node` entier.
///
/// CHECKPOINT MODÉRATION #2 (réception) : uniquement `moderation.is_blocked`
/// (CID directement listé par une denylist). Il n'y a PLUS de dérivé
/// "CIDs connus d'une clé bannie" (retiré, finding critique I2) — lister un
/// CID dans son feed ne prouve rien sur qui le détient réellement ; bloquer
/// aveuglément sur cette base permettrait à une clé bannie de faire
/// disparaître le contenu d'un tiers innocent en le mentionnant. La clé
/// bannie elle-même reste sans effet (son feed n'entre jamais au catalogue,
/// cf. `fetch_feed_inner`/`handle_feed_message`, et son stock connu est purgé
/// par `purge_blocked_issuer`) ; un CID individuel reste bloqué uniquement
/// s'il figure explicitement dans une denylist.
#[allow(clippy::too_many_arguments)]
async fn get_with_inner(
    blockstore: &Blockstore,
    moderation: &Arc<RwLock<Moderation>>,
    cmd_tx: &mpsc::Sender<Command>,
    self_peer_id: PeerId,
    keypair: &Keypair,
    reports: &Arc<Mutex<ReportBook>>,
    cid: Cid,
    policy: StorePolicy,
) -> CoreResult<Vec<u8>> {
    if is_blocked_inner(moderation, &cid) {
        emit_report_inner(cmd_tx, reports, keypair, &cid, "denylist").await;
        return Err(CoreError::Moderated(cid.to_string()));
    }
    let mut repairing_corruption = false;
    if blockstore.has(&cid) {
        match blockstore.get(&cid) {
            Ok(bytes) => return Ok(bytes),
            Err(CoreError::IntegrityMismatch) => {
                tracing::warn!("bloc local corrompu, re-téléchargement: {cid}");
                repairing_corruption = true;
            }
            Err(e) => return Err(e),
        }
    }
    let providers = get_providers_inner(cmd_tx, cid).await?;
    if providers.is_empty() {
        return Err(CoreError::NoProviders(cid.to_string()));
    }

    let mut inflight: FuturesUnordered<_> = providers
        .into_iter()
        .filter(|peer| *peer != self_peer_id)
        .map(|peer| request_block_inner(cmd_tx, peer, cid))
        .collect();
    while let Some(result) = inflight.next().await {
        match result {
            Ok(bytes) => {
                if repairing_corruption || policy == StorePolicy::Seed {
                    blockstore.put(&bytes)?;
                    let _ = provide_inner(cmd_tx, moderation, cid).await;
                }
                return Ok(bytes);
            }
            Err(e) => tracing::debug!("fournisseur rejeté pour {cid}: {e}"),
        }
    }
    Err(CoreError::BlockNotFound(cid.to_string()))
}

/// Repli de récupération froide (ADR 0008, CS-a tâche 3) : appelé par
/// `Node::get_with` uniquement quand `get_with_inner` a épuisé le P2P
/// (`NoProviders`). `retrieve` a déjà vérifié le CID (spec « repli, pas un
/// CDN ») ; re-vérifier via `blockstore.put` (content-addressed) reste
/// gratuit et défensif. `Ok(None)` (introuvable au froid aussi) propage
/// `NoProviders` — jamais une erreur pour une simple absence.
///
/// CHECKPOINT MODÉRATION #2 : identique au chemin P2P — un contenu matché
/// n'est ni mis en cache ni fourni, quelle que soit `policy`, et le refus
/// est signalé (best-effort) aux pairs.
#[cfg(feature = "cold-storage")]
#[allow(clippy::too_many_arguments)]
async fn cold_fallback_inner(
    blockstore: &Blockstore,
    moderation: &Arc<RwLock<Moderation>>,
    cmd_tx: &mpsc::Sender<Command>,
    reports: &Arc<Mutex<ReportBook>>,
    keypair: &Keypair,
    cold: &Arc<dyn ColdStore>,
    cid: Cid,
    policy: StorePolicy,
) -> CoreResult<Vec<u8>> {
    let Some(bytes) = cold.retrieve(cid).await? else {
        return Err(CoreError::NoProviders(cid.to_string()));
    };
    if is_blocked_inner(moderation, &cid) {
        emit_report_inner(cmd_tx, reports, keypair, &cid, "denylist").await;
        return Err(CoreError::Moderated(cid.to_string()));
    }
    // Défense en profondeur : `cold` est un `dyn ColdStore` — la vérification
    // du CID ne doit pas dépendre de la discipline interne d'une implémentation
    // tierce. `retrieve` est censé l'avoir déjà fait (spec), mais le point de
    // service (surtout `Stream`, qui ne repasse PAS par `blockstore.put`,
    // content-addressed) ne doit JAMAIS rendre des octets non vérifiés — un
    // backend froid menteur ou bogué est traité comme un fournisseur absent.
    if !verify(&cid, &bytes) {
        tracing::warn!("backend froid: octets ne correspondant pas au CID {cid}, rejetés");
        return Err(CoreError::NoProviders(cid.to_string()));
    }
    if policy == StorePolicy::Seed {
        blockstore.put(&bytes)?;
        let _ = provide_inner(cmd_tx, moderation, cid).await;
    }
    Ok(bytes)
}

/// Supprime du magasin les blocs de `publications` qui ne sont référencés par
/// AUCUNE publication encore retenue dans `seed_index` (vérifié via
/// `all_cids` — un segment peut en théorie être partagé entre publications).
/// Utilisé après une purge/éviction, jamais avant : `seed_index` doit déjà
/// refléter l'état post-retrait au moment de l'appel.
fn remove_unshared_blocks(
    blockstore: &Blockstore,
    seed_index: &Arc<Mutex<SeedIndex>>,
    publications: &[SeededPublication],
) {
    let remaining: HashSet<String> = {
        let idx = seed_index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        idx.all_cids().into_iter().collect()
    };
    for publication in publications {
        let mut cids = vec![publication.manifest_cid.clone()];
        cids.extend(publication.segment_cids.iter().cloned());
        for cid_str in cids {
            if remaining.contains(&cid_str) {
                continue;
            }
            if let Ok(cid) = cid_str.parse::<Cid>() {
                let _ = blockstore.remove(&cid);
            }
        }
    }
}

/// État partagé de la boucle de seed proactif (lot c) : des `Arc`/clones
/// isolés, jamais un `Node` entier (voir le commentaire sur `alive` /
/// `follow_loop` — un `Node` fort empêcherait la boucle d'évènements de
/// jamais s'arrêter).
struct SeedLoopState {
    cmd_tx: mpsc::Sender<Command>,
    blockstore: Blockstore,
    moderation: Arc<RwLock<Moderation>>,
    catalog: Arc<Mutex<Catalog>>,
    subscriptions: Arc<Mutex<BTreeSet<PeerId>>>,
    seed_index: Arc<Mutex<SeedIndex>>,
    seed_quota: Arc<Mutex<u64>>,
    seed_events: tokio::sync::broadcast::Sender<()>,
    reports: Arc<Mutex<ReportBook>>,
    keypair: Keypair,
    peer_id: PeerId,
    /// Manifestes déjà tentés et sautés faute de place sous le quota courant
    /// (en mémoire seulement, jamais persisté). Évite de re-tenter — donc de
    /// re-fetcher-puis-annuler — la même publication doomed à chaque tic :
    /// gaspillage réseau, et source d'un flake observé en test (le rollback
    /// laisse une fenêtre où le bloc réapparaît juste avant d'être retiré à
    /// nouveau). Vidé par `seed_loop` chaque fois qu'un évènement (catalogue
    /// ou quota) suggère que la situation a pu changer — pas sur un simple
    /// tic d'intervalle, qui ne change rien à lui seul.
    quota_blocked: Arc<Mutex<HashSet<String>>>,
}

/// Fait de la place pour `extra` octets supplémentaires sous le quota
/// courant, au profit d'un candidat dont la réplication mesurée est
/// `candidate_replication` : si `total_bytes() + extra` dépasse déjà le
/// quota, évince (ordre [`eviction_order`], réplication mesurée en DHT pour
/// chaque candidat évictable) jusqu'à rentrer dans le quota. Renvoie `false`
/// si, même après avoir épuisé tous les candidats évictables (non épinglés),
/// ça ne rentre toujours pas — la publication en cours doit alors être
/// sautée.
///
/// **Dampening anti-oscillation** (retour de review) : une éviction n'a lieu
/// que si la victime potentielle a une réplication STRICTEMENT supérieure à
/// celle du candidat (`victime > candidat`, jamais `>=`). Sans ce garde-fou,
/// deux publications à réplication ÉGALE et un quota qui n'en tient qu'une
/// s'évincent mutuellement à chaque passe (P2 évince P1, la passe suivante
/// visite les manifestes dans un ordre différent, P1 évince P2, etc. —
/// re-téléchargement sans fin, `quota_blocked` ne s'engage jamais puisque
/// chaque tentative individuelle "réussit" en évinçant l'autre). Avec le
/// garde-fou : à réplication égale (ou si le candidat est LUI-MÊME mieux
/// répliqué que la victime potentielle, donc moins légitime à la garder),
/// aucune éviction n'a lieu — le candidat est sauté et mémorisé dans
/// `quota_blocked` par l'appelant, ce qui fixe le résultat de la passe
/// (premier arrivé, premier servi) jusqu'au prochain évènement catalogue/quota.
async fn make_room_for(state: &SeedLoopState, extra: u64, candidate_replication: usize) -> bool {
    let quota = *state
        .seed_quota
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    loop {
        let used = state
            .seed_index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .total_bytes();
        // Strict : `extra` (déjà engagé par la publication en cours, pas
        // encore indexé) doit laisser AU MOINS un peu de marge, faute de quoi
        // le prochain bloc (de taille encore inconnue) ferait dépasser le
        // quota à coup sûr. `<=` laisserait passer un quota tout juste
        // consommé par ce qui est déjà en vol.
        if used.saturating_add(extra) < quota {
            return true;
        }
        let candidates: Vec<String> = {
            let idx = state
                .seed_index
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            eviction_order(&idx, &HashMap::new())
                .into_iter()
                .map(|p| p.manifest_cid.clone())
                .collect()
        };
        if candidates.is_empty() {
            return false;
        }
        // Réplication mesurée en DHT pour CHAQUE candidat évictable (spec :
        // l'éviction préfère faire partir ce qui est déjà bien répliqué
        // ailleurs) — recalculée à chaque tour puisque l'ensemble change.
        let mut replication = HashMap::new();
        for manifest_cid in &candidates {
            if let Ok(cid) = manifest_cid.parse::<Cid>() {
                let count = get_providers_inner(&state.cmd_tx, cid)
                    .await
                    .map(|peers| peers.len())
                    .unwrap_or(0);
                replication.insert(manifest_cid.clone(), count);
            }
        }
        let victim = {
            let idx = state
                .seed_index
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            eviction_order(&idx, &replication)
                .first()
                .map(|p| p.manifest_cid.clone())
        };
        let Some(victim) = victim else {
            return false;
        };
        let victim_replication = replication.get(&victim).copied().unwrap_or(0);
        if victim_replication <= candidate_replication {
            // La victime potentielle n'est pas MOINS légitime à rester
            // seedée que le candidat (réplication égale ou même inférieure)
            // — pas d'éviction. Voir le commentaire de dampening ci-dessus.
            return false;
        }
        evict_publication(state, &victim);
    }
}

/// Retire une publication du SeedIndex et ses blocs devenus orphelins.
fn evict_publication(state: &SeedLoopState, manifest_cid: &str) {
    let removed = {
        let mut idx = state
            .seed_index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let removed = idx.remove_publication(manifest_cid);
        if removed.is_some() {
            if let Err(e) = seeding::save_seed_index(&state.blockstore, &idx) {
                tracing::warn!("persistance de l'index de seed échouée: {e}");
            }
        }
        removed
    };
    if let Some((_, publication)) = removed {
        remove_unshared_blocks(&state.blockstore, &state.seed_index, &[publication]);
    }
}

/// Récupère et retient (SeedIndex) une publication (manifeste + segments)
/// sous quota : manifeste puis chaque segment manquant, chacun via
/// `get_with_inner(Seed)` (mise en cache + annonce), en faisant de la place
/// (`make_room_for`) avant chaque récupération qui ferait grossir le magasin.
/// Si aucune éviction ne suffit à faire de la place, la publication est
/// abandonnée : tout bloc déjà récupéré pour elle pendant cette tentative
/// (jamais indexé, donc jamais compté dans `total_bytes`) est retiré du
/// magasin — pas d'accumulation silencieuse hors comptabilité.
/// Décision du point de commit du seed proactif, isolée en fonction PURE pour
/// être testable sans simuler la course TOCTOU (finding revue finale lot d) :
/// une publication fraîchement seedée n'est committée à l'index QUE si son
/// émetteur est toujours abonné ET n'est pas banni par denylist. Le ban prime
/// sur l'abonnement (`subscribe_denylist` ne désabonne pas — sans ce «&& !banni»
/// une publication d'une clé bannie en cours de seed survivrait à la purge) ;
/// le blocage local est déjà couvert par l'abonnement (`block_channel` désabonne).
fn seed_still_wanted(subscribed: bool, key_banned: bool) -> bool {
    subscribed && !key_banned
}

async fn seed_publication(
    state: &SeedLoopState,
    issuer: PeerId,
    manifest_cid: Cid,
) -> CoreResult<()> {
    // Réplication du candidat lui-même, mesurée UNE fois et réutilisée pour
    // tout le reste de cette tentative (manifeste + chaque segment) — le
    // dampening anti-oscillation de `make_room_for` compare la victime
    // potentielle à CE candidat, pas à un segment particulier.
    let candidate_replication = get_providers_inner(&state.cmd_tx, manifest_cid)
        .await
        .map(|peers| peers.len())
        .unwrap_or(0);
    if !make_room_for(state, 0, candidate_replication).await {
        // Même le manifeste ne rentre pas et rien n'est évictable (ou rien
        // d'assez répliqué pour justifier une éviction) : on ne tente même
        // pas la récupération réseau.
        mark_quota_blocked(state, manifest_cid);
        return Ok(());
    }
    let manifest_bytes = get_with_inner(
        &state.blockstore,
        &state.moderation,
        &state.cmd_tx,
        state.peer_id,
        &state.keypair,
        &state.reports,
        manifest_cid,
        StorePolicy::Seed,
    )
    .await?;
    let manifest = HlsManifest::from_json(&manifest_bytes)?;
    let mut fetched_cids = vec![manifest_cid.to_string()];
    // Octets déjà engagés par CETTE tentative, pas encore comptés dans
    // `seed_index.total_bytes()` (l'insertion n'a lieu qu'en fin de fonction,
    // une fois la publication complète) — passés en `extra` à `make_room_for`
    // pour que le quota tienne compte de ce qui est déjà en train d'être
    // récupéré, pas seulement de ce qui est déjà indexé.
    let mut fetched_bytes = state.blockstore.size_of(&manifest_cid).unwrap_or(0);

    for seg in &manifest.segments {
        let cid: Cid = match seg.cid.parse() {
            Ok(c) => c,
            Err(e) => {
                remove_unindexed_fetch(state, &fetched_cids);
                return Err(CoreError::Cid(e));
            }
        };
        if !state.blockstore.has(&cid)
            && !make_room_for(state, fetched_bytes, candidate_replication).await
        {
            // Plus de place et rien à évincer : publication sautée, on
            // nettoie ce qui a été récupéré pour elle jusqu'ici.
            remove_unindexed_fetch(state, &fetched_cids);
            mark_quota_blocked(state, manifest_cid);
            return Ok(());
        }
        match get_with_inner(
            &state.blockstore,
            &state.moderation,
            &state.cmd_tx,
            state.peer_id,
            &state.keypair,
            &state.reports,
            cid,
            StorePolicy::Seed,
        )
        .await
        {
            Ok(_) => {
                fetched_cids.push(cid.to_string());
                fetched_bytes += state.blockstore.size_of(&cid).unwrap_or(0);
            }
            Err(e) => {
                remove_unindexed_fetch(state, &fetched_cids);
                return Err(e);
            }
        }
    }

    let total_bytes = fetched_bytes;
    let segment_cids: Vec<String> = manifest.segments.iter().map(|s| s.cid.clone()).collect();
    // Point de COMMIT : les fetchs réseau ci-dessus ont pu durer — entre-temps
    // un `register_seeded_publication` (lecture au premier plan) a pu indexer
    // la même publication, ou un désabonnement/blocage/ban a pu purger
    // l'émetteur. Sans re-validation ici, une insertion retardataire
    // ressusciterait dans l'index une publication déjà purgée (métadonnées
    // orphelines, stats faussées, blocs réannoncés — course observée en test).
    // Deux verdicts d'invalidation : (1) plus abonné — couvre désabonnement ET
    // blocage local (`block_channel` désabonne) ; (2) clé bannie par denylist —
    // `subscribe_denylist` NE désabonne PAS, donc `subs.contains` resterait
    // vrai : sans ce second test, une publication d'une clé bannie en cours de
    // seed survivrait à la purge rétroactive (TOCTOU, finding revue finale d).
    // Ordre des verrous : abonnements PUIS index, comme partout ailleurs.
    let key_banned = state
        .moderation
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_blocked_key(&issuer);
    let committed = {
        let subs = state
            .subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !seed_still_wanted(subs.contains(&issuer), key_banned) {
            false
        } else {
            let mut idx = state
                .seed_index
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // Doublon (l'autre chemin a gagné la course) : les blocs sont
            // partagés avec la copie indexée, rien à insérer ni à retirer.
            if !idx.contains_manifest(&manifest_cid.to_string()) {
                idx.insert(
                    issuer.to_string(),
                    SeededPublication {
                        manifest_cid: manifest_cid.to_string(),
                        segment_cids,
                        total_bytes,
                        order: 0, // ignoré par `insert`, réassigné par l'index.
                    },
                );
                if let Err(e) = seeding::save_seed_index(&state.blockstore, &idx) {
                    tracing::warn!("persistance de l'index de seed échouée: {e}");
                }
            }
            true
        }
    };
    if !committed {
        // L'émetteur n'est plus suivi : la tentative est abandonnée et ses
        // blocs non indexés sont retirés (ceux partagés avec une publication
        // indexée survivent via la garde `all_cids` de `remove_unindexed_fetch`).
        remove_unindexed_fetch(state, &fetched_cids);
        return Ok(());
    }
    let _ = state.seed_events.send(());
    Ok(())
}

/// Mémorise qu'une publication a été sautée faute de place évictable, pour
/// éviter de la retenter à chaque tic tant que rien n'a changé (cf. le
/// commentaire sur `SeedLoopState::quota_blocked`).
fn mark_quota_blocked(state: &SeedLoopState, manifest_cid: Cid) {
    state
        .quota_blocked
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(manifest_cid.to_string());
}

/// Retire du magasin les blocs récupérés pour une tentative de publication
/// abandonnée — ils ne sont jamais entrés dans `seed_index` (l'insertion n'a
/// lieu qu'en fin de `seed_publication`), donc `remove_unshared_blocks`
/// (qui vérifie contre `seed_index.all_cids()`) les retire tous, sauf ceux
/// qu'une AUTRE publication déjà indexée référencerait par ailleurs.
fn remove_unindexed_fetch(state: &SeedLoopState, fetched_cids: &[String]) {
    let placeholder = SeededPublication {
        manifest_cid: fetched_cids.first().cloned().unwrap_or_default(),
        segment_cids: fetched_cids.iter().skip(1).cloned().collect(),
        total_bytes: 0,
        order: 0,
    };
    remove_unshared_blocks(&state.blockstore, &state.seed_index, &[placeholder]);
}

/// Seed (au sens `seed_publication`) les publications non encore retenues du
/// feed courant d'un émetteur, en itérant `entry.cids` en ordre INVERSE.
///
/// **Pas de garantie temporelle** (correction post-review) : `entry.cids`
/// vient de `feed.entries`, dans l'ordre où l'émetteur les a passées à
/// `publish_feed`/`publish_feed_with` — rien dans `feed.rs`/`catalog.rs` ne
/// contraint cet ordre à être chronologique. En pratique, le chemin
/// « publier tout » de `champinium-cli` (`publish`/`republish`) construit ses
/// CIDs depuis `Blockstore::list()`, qui énumère `std::fs::read_dir` SANS
/// tri — un ordre de système de fichiers, sans rapport avec l'ordre
/// d'ingestion. L'inversion ici est donc une heuristique (« si quelqu'un
/// republie systématiquement en ajoutant en fin de liste, ça favorise le
/// contenu récent ») et PAS une garantie « le plus récent d'abord ». Un vrai
/// ordre de récence nécessiterait un horodatage porté par `FeedEntry` — hors
/// périmètre de cette tâche, noté comme suite possible dans le rapport.
async fn seed_channel(state: &SeedLoopState, issuer: PeerId) {
    let entry = {
        let cat = state
            .catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cat.entries().into_iter().find(|e| e.issuer == issuer)
    };
    let Some(entry) = entry else { return };
    for manifest_cid in entry.cids.into_iter().rev() {
        let manifest_cid_str = manifest_cid.to_string();
        let already_seeded = state
            .seed_index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_manifest(&manifest_cid_str);
        if already_seeded {
            continue;
        }
        let known_blocked = state
            .quota_blocked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&manifest_cid_str);
        if known_blocked {
            continue;
        }
        if let Err(e) = seed_publication(state, issuer, manifest_cid).await {
            tracing::debug!("seed proactif: échec pour {manifest_cid} ({issuer}): {e}");
        }
    }
}

/// Une passe complète : round-robin sur les channels souscrits (démarre à un
/// index différent à chaque appel — `round_robin`, renvoyé mis à jour — pour
/// qu'un channel gourmand en publications manquantes ne prive pas
/// systématiquement les autres d'un tour).
async fn seed_pass(state: &SeedLoopState, round_robin: usize) -> usize {
    let issuers: Vec<PeerId> = {
        let subs = state
            .subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        subs.iter().copied().collect()
    };
    if issuers.is_empty() {
        return 0;
    }
    let start = round_robin % issuers.len();
    for offset in 0..issuers.len() {
        let issuer = issuers[(start + offset) % issuers.len()];
        seed_channel(state, issuer).await;
    }
    round_robin.wrapping_add(1)
}

/// Boucle de seed proactif des channels souscrits (spec channels lot c) : une
/// passe (`seed_pass`) à chaque itération, puis attend soit `seed_interval`,
/// soit un évènement catalogue (nouvelle publication à seeder promptement),
/// soit un réveil explicite (`seed_wake`, ex. changement de quota via
/// `Node::set_seed_quota`). Comme `follow_loop` : la toute première passe a
/// lieu AVANT toute attente (rattrapage au démarrage), et la boucle ne tient
/// qu'un [`Weak`] marqueur de vivacité — jamais un `Node` fort.
async fn seed_loop(
    alive: Weak<()>,
    state: SeedLoopState,
    mut catalog_events: tokio::sync::broadcast::Receiver<()>,
    mut seed_wake: tokio::sync::broadcast::Receiver<()>,
    seed_interval: Duration,
) {
    let mut round_robin: usize = 0;
    loop {
        if alive.upgrade().is_none() {
            return;
        }
        round_robin = seed_pass(&state, round_robin).await;
        tokio::select! {
            _ = tokio::time::sleep(seed_interval) => {}
            // Catalogue ou quota ont pu changer ce qui tenait avant du quota
            // dépassé (nouvelle éviction possible, quota relevé…) — un
            // simple tic d'intervalle, lui, ne change rien à lui seul : on ne
            // vide `quota_blocked` que sur ces deux branches, pas sur le
            // sleep (évite de re-fetcher-puis-annuler la même publication
            // doomed à chaque tic).
            _ = catalog_events.recv() => {
                state.quota_blocked.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
            }
            _ = seed_wake.recv() => {
                state.quota_blocked.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
            }
        }
    }
}

/// Préfixe des clés DHT de feeds.
const FEED_KEY_PREFIX: &[u8] = b"/champinium/feed/";

/// Préfixe des clés de fournisseurs de tags.
const TAG_KEY_PREFIX: &[u8] = b"/champinium/tag/";

/// Nombre max de tags distincts annoncés par publication (anti-abus : chaque
/// annonce est une requête DHT).
const MAX_PROVIDED_TAGS: usize = 64;

/// Clé de fournisseur d'un tag : `/champinium/tag/<tag normalisé>`.
fn tag_provider_key(tag: &str) -> RecordKey {
    let mut key = TAG_KEY_PREFIX.to_vec();
    key.extend_from_slice(tag.as_bytes());
    RecordKey::new(&key)
}

/// Clé DHT du feed d'un créateur : `/champinium/feed/<peerid>`.
fn feed_record_key(peer: &PeerId) -> RecordKey {
    let mut key = FEED_KEY_PREFIX.to_vec();
    key.extend_from_slice(&peer.to_bytes());
    RecordKey::new(&key)
}

/// Valide un record DHT entrant avant stockage : la clé doit être une clé de
/// feed, la valeur un feed signé dont l'émetteur correspond au `<peerid>` de la
/// clé. Toute autre clé est refusée (aucun autre type de record applicatif).
fn is_valid_feed_record(record: &kad::Record) -> bool {
    let Some(peer_bytes) = record.key.as_ref().strip_prefix(FEED_KEY_PREFIX) else {
        return false;
    };
    let Ok(expected_issuer) = PeerId::from_bytes(peer_bytes) else {
        return false;
    };
    let Ok(feed) = Feed::from_json(&record.value) else {
        return false;
    };
    feed.verify().is_ok() && feed.issuer_peer_id().ok() == Some(expected_issuer)
}

/// Sépare une multiaddr terminée par `/p2p/<peerid>` en `(PeerId, addr de base)`.
pub fn split_peer_id(mut addr: Multiaddr) -> CoreResult<(PeerId, Multiaddr)> {
    match addr.pop() {
        Some(libp2p::multiaddr::Protocol::P2p(peer)) => Ok((peer, addr)),
        _ => Err(CoreError::Network(
            "adresse sans composant /p2p/<peerid>".into(),
        )),
    }
}

fn build_swarm(keypair: Keypair) -> CoreResult<Swarm<Behaviour>> {
    let swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .map_err(|e| CoreError::Network(e.to_string()))?
        .with_relay_client(noise::Config::new, yamux::Config::default)
        .map_err(|e| CoreError::Network(e.to_string()))?
        .with_behaviour(|key, relay_client| {
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(Behaviour::new(key, relay_client))
        })
        .map_err(|e| CoreError::Network(e.to_string()))?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();
    Ok(swarm)
}

struct EventLoop {
    swarm: Swarm<Behaviour>,
    blockstore: Blockstore,
    moderation: Arc<RwLock<Moderation>>,
    catalog: Arc<Mutex<Catalog>>,
    catalog_events: tokio::sync::broadcast::Sender<()>,
    reports: Arc<Mutex<ReportBook>>,
    subscriptions: Arc<Mutex<BTreeSet<PeerId>>>,
    /// Voir la doc du champ `Node::blocked_channels`.
    blocked_channels: Arc<Mutex<BTreeSet<PeerId>>>,
    feeds_topic: gossipsub::IdentTopic,
    reports_topic: gossipsub::IdentTopic,
    cmd_rx: mpsc::Receiver<Command>,
    pending_listen:
        HashMap<libp2p::core::transport::ListenerId, oneshot::Sender<CoreResult<Multiaddr>>>,
    pending_provide: HashMap<QueryId, oneshot::Sender<CoreResult<()>>>,
    pending_get_providers: HashMap<QueryId, (oneshot::Sender<HashSet<PeerId>>, HashSet<PeerId>)>,
    pending_request: HashMap<OutboundRequestId, oneshot::Sender<CoreResult<Vec<u8>>>>,
    pending_put_record: HashMap<QueryId, oneshot::Sender<CoreResult<()>>>,
    pending_get_record: HashMap<QueryId, PendingGetRecord>,
}

/// Émetteur de résultat + valeurs accumulées pour une requête GET de record DHT.
type PendingGetRecord = (oneshot::Sender<Vec<Vec<u8>>>, Vec<Vec<u8>>);

impl EventLoop {
    #[allow(clippy::too_many_arguments)]
    fn new(
        swarm: Swarm<Behaviour>,
        blockstore: Blockstore,
        moderation: Arc<RwLock<Moderation>>,
        catalog: Arc<Mutex<Catalog>>,
        catalog_events: tokio::sync::broadcast::Sender<()>,
        reports: Arc<Mutex<ReportBook>>,
        subscriptions: Arc<Mutex<BTreeSet<PeerId>>>,
        blocked_channels: Arc<Mutex<BTreeSet<PeerId>>>,
        feeds_topic: gossipsub::IdentTopic,
        reports_topic: gossipsub::IdentTopic,
        cmd_rx: mpsc::Receiver<Command>,
    ) -> Self {
        Self {
            swarm,
            blockstore,
            moderation,
            catalog,
            catalog_events,
            reports,
            subscriptions,
            blocked_channels,
            feeds_topic,
            reports_topic,
            cmd_rx,
            pending_listen: HashMap::new(),
            pending_provide: HashMap::new(),
            pending_get_providers: HashMap::new(),
            pending_request: HashMap::new(),
            pending_put_record: HashMap::new(),
            pending_get_record: HashMap::new(),
        }
    }

    async fn run(mut self) {
        loop {
            tokio::select! {
                cmd = self.cmd_rx.recv() => match cmd {
                    Some(cmd) => self.handle_command(cmd),
                    None => break, // toutes les poignées Node sont tombées
                },
                event = self.swarm.select_next_some() => self.handle_event(event),
            }
        }
    }

    fn handle_command(&mut self, cmd: Command) {
        match cmd {
            Command::Listen { addr, tx } => match self.swarm.listen_on(addr) {
                // Corréler par ListenerId : un `listen_on` sur 0.0.0.0 émet
                // plusieurs NewListenAddr (une par interface) et deux `listen`
                // concurrents ne doivent pas croiser leurs réponses.
                Ok(listener_id) => {
                    self.pending_listen.insert(listener_id, tx);
                }
                Err(e) => {
                    let _ = tx.send(Err(CoreError::Network(e.to_string())));
                }
            },
            Command::Dial { addr, tx } => {
                let res = self
                    .swarm
                    .dial(addr)
                    .map_err(|e| CoreError::Network(e.to_string()));
                let _ = tx.send(res);
            }
            Command::AddAddress { peer, addr } => {
                self.swarm.behaviour_mut().kademlia.add_address(&peer, addr);
            }
            Command::Provide { key, tx } => {
                match self.swarm.behaviour_mut().kademlia.start_providing(key) {
                    Ok(qid) => {
                        self.pending_provide.insert(qid, tx);
                    }
                    Err(e) => {
                        let _ = tx.send(Err(CoreError::Network(e.to_string())));
                    }
                }
            }
            Command::GetProviders { key, tx } => {
                let qid = self.swarm.behaviour_mut().kademlia.get_providers(key);
                self.pending_get_providers.insert(qid, (tx, HashSet::new()));
            }
            Command::RequestBlock { peer, cid, tx } => {
                let rid = self
                    .swarm
                    .behaviour_mut()
                    .blocks
                    .send_request(&peer, BlockRequest(cid.to_bytes()));
                self.pending_request.insert(rid, tx);
            }
            Command::ListenAddrs { tx } => {
                let _ = tx.send(self.swarm.listeners().cloned().collect());
            }
            Command::PublishFeed { data, tx } => {
                let res = match self
                    .swarm
                    .behaviour_mut()
                    .gossipsub
                    .publish(self.feeds_topic.clone(), data)
                {
                    Ok(_) => Ok(()),
                    // Pas encore de pairs dans le mesh : pas une erreur fatale, le
                    // feed reste dans le catalogue local et sera rediffusé.
                    Err(gossipsub::PublishError::NoPeersSubscribedToTopic) => Ok(()),
                    Err(e) => Err(CoreError::Network(format!("gossipsub publish: {e}"))),
                };
                let _ = tx.send(res);
            }
            Command::PutRecord { key, value, tx } => {
                let record = kad::Record::new(key, value);
                match self
                    .swarm
                    .behaviour_mut()
                    .kademlia
                    .put_record(record, kad::Quorum::One)
                {
                    Ok(qid) => {
                        self.pending_put_record.insert(qid, tx);
                    }
                    Err(e) => {
                        let _ = tx.send(Err(CoreError::Network(e.to_string())));
                    }
                }
            }
            Command::GetRecord { key, tx } => {
                let qid = self.swarm.behaviour_mut().kademlia.get_record(key);
                self.pending_get_record.insert(qid, (tx, Vec::new()));
            }
            Command::PeerScore { peer, tx } => {
                let _ = tx.send(self.swarm.behaviour().gossipsub.peer_score(&peer));
            }
            Command::PublishReport { data, tx } => {
                let res = match self
                    .swarm
                    .behaviour_mut()
                    .gossipsub
                    .publish(self.reports_topic.clone(), data)
                {
                    Ok(_) => Ok(()),
                    // Personne n'écoute encore : le signalement est best-effort.
                    Err(gossipsub::PublishError::NoPeersSubscribedToTopic) => Ok(()),
                    Err(e) => Err(CoreError::Network(format!("gossipsub publish: {e}"))),
                };
                let _ = tx.send(res);
            }
            Command::StopProviding { key } => {
                self.swarm.behaviour_mut().kademlia.stop_providing(&key);
            }
        }
    }

    fn handle_event(&mut self, event: SwarmEvent<BehaviourEvent>) {
        match event {
            SwarmEvent::NewListenAddr {
                listener_id,
                address,
            } => {
                // Répond au premier NewListenAddr de ce listener (les suivants,
                // autres interfaces, n'ont plus d'attente à satisfaire).
                if let Some(tx) = self.pending_listen.remove(&listener_id) {
                    let _ = tx.send(Ok(address));
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received {
                peer_id,
                info,
                ..
            })) => {
                // Peuple la table de routage avec les adresses annoncées par le pair.
                for addr in info.listen_addrs {
                    self.swarm
                        .behaviour_mut()
                        .kademlia
                        .add_address(&peer_id, addr);
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::Kademlia(
                kad::Event::OutboundQueryProgressed {
                    id, result, step, ..
                },
            )) => self.handle_kad_result(id, result, step.last),
            SwarmEvent::Behaviour(BehaviourEvent::Kademlia(kad::Event::InboundRequest {
                request,
            })) => self.handle_inbound_kad_request(request),
            SwarmEvent::Behaviour(BehaviourEvent::Blocks(request_response::Event::Message {
                message,
                ..
            })) => self.handle_block_message(message),
            SwarmEvent::Behaviour(BehaviourEvent::Blocks(
                request_response::Event::OutboundFailure {
                    request_id, error, ..
                },
            )) => {
                if let Some(tx) = self.pending_request.remove(&request_id) {
                    let _ = tx.send(Err(CoreError::Network(error.to_string())));
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::Gossipsub(gossipsub::Event::Message {
                propagation_source,
                message_id,
                message,
            })) => {
                // Dispatch par topic : feeds (catalogue) ou signalements.
                if message.topic == self.reports_topic.hash() {
                    self.handle_report_message(&message_id, &propagation_source, &message.data);
                } else {
                    self.handle_feed_message(&message_id, &propagation_source, &message.data);
                }
            }
            _ => {}
        }
    }

    /// Applique un rapport de signalement reçu (signature vérifiée) à l'agrégat
    /// local, puis rapporte le verdict à gossipsub (mêmes règles que les feeds :
    /// Accept si l'agrégat change, Ignore si sans effet — doublon, agrégat
    /// plein —, Reject si invalide → pénalise l'émetteur via le peer scoring).
    fn handle_report_message(
        &mut self,
        message_id: &gossipsub::MessageId,
        propagation_source: &PeerId,
        data: &[u8],
    ) {
        let acceptance = match Report::from_json(data).and_then(|r| r.verify().map(|()| r)) {
            Ok(report) => {
                let mut book = self
                    .reports
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match book.apply(&report) {
                    Ok(true) => gossipsub::MessageAcceptance::Accept,
                    Ok(false) => gossipsub::MessageAcceptance::Ignore,
                    Err(e) => {
                        tracing::debug!("rapport rejeté: {e}");
                        gossipsub::MessageAcceptance::Reject
                    }
                }
            }
            Err(e) => {
                tracing::debug!("rapport invalide: {e}");
                gossipsub::MessageAcceptance::Reject
            }
        };
        self.swarm
            .behaviour_mut()
            .gossipsub
            .report_message_validation_result(message_id, propagation_source, acceptance);
    }

    /// Stores DHT entrants (mode `StoreInserts::FilterBoth` : rien n'est stocké
    /// automatiquement). Seul un record de feed **valide** (signé, cohérent avec
    /// la clé `/champinium/feed/<peerid>`) est stocké — un tiers ne peut donc pas
    /// écraser le record d'un créateur. Les provider records sont de simples
    /// annonces (le contenu est vérifié par CID au téléchargement) : acceptés.
    fn handle_inbound_kad_request(&mut self, request: kad::InboundRequest) {
        match request {
            kad::InboundRequest::PutRecord {
                record: Some(record),
                ..
            } => {
                if is_valid_feed_record(&record) {
                    if let Err(e) = self.swarm.behaviour_mut().kademlia.store_mut().put(record) {
                        tracing::debug!("record DHT refusé par le store: {e}");
                    }
                } else {
                    tracing::debug!("record DHT invalide ignoré");
                }
            }
            kad::InboundRequest::AddProvider {
                record: Some(record),
            } => {
                if let Err(e) = self
                    .swarm
                    .behaviour_mut()
                    .kademlia
                    .store_mut()
                    .add_provider(record)
                {
                    tracing::debug!("provider record refusé par le store: {e}");
                }
            }
            _ => {}
        }
    }

    /// Applique un feed reçu en gossipsub au catalogue local (signature vérifiée
    /// dans `Catalog::apply`) puis rapporte le verdict à gossipsub
    /// (`validate_messages` est actif : sans rapport, rien n'est relayé) :
    /// - feed appliqué → **Accept** (relayé) ;
    /// - feed valide mais sans effet (seq déjà vu, catalogue plein) → **Ignore**
    ///   (pas relayé, pas pénalisé : c'est du retard, pas de la malveillance) ;
    /// - illisible ou signature invalide → **Reject** (pénalise l'émetteur via
    ///   le peer scoring).
    ///
    /// CHECKPOINT MODÉRATION (ingestion catalogue, finding M2) : un feed dont
    /// l'émetteur est une clé bloquée (denylist OU blocage local, tâche 3)
    /// n'entre JAMAIS au catalogue, même signature valide — vérifiée EN
    /// PREMIER (`feed.verify()`) pour ne jamais fonder ce check sur un
    /// `issuer_pubkey` non authentifié. Verdict **Reject** : le peer scoring
    /// gossipsub décourage déjà le relais de cette clé, pas besoin d'un
    /// rapport P2P par CID en plus (sobriété — le rapport existant reste
    /// déclenché par le checkpoint #2 sur les contenus, pas ici).
    ///
    /// Ne capture PLUS les CIDs listés par ce feed dans un registre dérivé
    /// (retiré, finding critique I2) : le contenu de ce feed n'est de toute
    /// façon jamais appliqué au catalogue, donc jamais atteignable via ce
    /// nœud — capturer ses CIDs pour les bloquer ailleurs aurait permis à la
    /// clé bannie de faire disparaître le contenu d'un tiers innocent
    /// simplement en le mentionnant dans son propre feed.
    fn handle_feed_message(
        &mut self,
        message_id: &gossipsub::MessageId,
        propagation_source: &PeerId,
        data: &[u8],
    ) {
        let acceptance = match Feed::from_json(data) {
            Ok(feed) => match feed.verify() {
                Ok(()) => match feed.issuer_peer_id() {
                    Ok(issuer)
                        if is_key_blocked_inner(
                            &self.moderation,
                            &self.blocked_channels,
                            &issuer,
                        ) =>
                    {
                        gossipsub::MessageAcceptance::Reject
                    }
                    Ok(_) => {
                        let subs: HashSet<PeerId> = self
                            .subscriptions
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .iter()
                            .copied()
                            .collect();
                        match self
                            .catalog
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .apply(feed, &subs)
                        {
                            Ok(true) => {
                                let _ = self.catalog_events.send(());
                                gossipsub::MessageAcceptance::Accept
                            }
                            Ok(false) => gossipsub::MessageAcceptance::Ignore,
                            Err(e) => {
                                tracing::debug!("feed rejeté: {e}");
                                gossipsub::MessageAcceptance::Reject
                            }
                        }
                    }
                    Err(e) => {
                        tracing::debug!("feed sans émetteur exploitable: {e}");
                        gossipsub::MessageAcceptance::Reject
                    }
                },
                Err(e) => {
                    tracing::debug!("feed invalide: {e}");
                    gossipsub::MessageAcceptance::Reject
                }
            },
            Err(e) => {
                tracing::debug!("feed illisible: {e}");
                gossipsub::MessageAcceptance::Reject
            }
        };
        self.swarm
            .behaviour_mut()
            .gossipsub
            .report_message_validation_result(message_id, propagation_source, acceptance);
    }

    fn handle_kad_result(&mut self, id: QueryId, result: QueryResult, last: bool) {
        match result {
            QueryResult::StartProviding(res) => {
                if let Some(tx) = self.pending_provide.remove(&id) {
                    let _ = tx.send(
                        res.map(|_| ())
                            .map_err(|e| CoreError::Network(e.to_string())),
                    );
                }
            }
            QueryResult::GetProviders(res) => {
                if let Ok(GetProvidersOk::FoundProviders { providers, .. }) = &res {
                    if let Some((_, acc)) = self.pending_get_providers.get_mut(&id) {
                        acc.extend(providers.iter().copied());
                    }
                }
                if last {
                    if let Some((tx, acc)) = self.pending_get_providers.remove(&id) {
                        let _ = tx.send(acc);
                    }
                }
            }
            QueryResult::PutRecord(res) => {
                if let Some(tx) = self.pending_put_record.remove(&id) {
                    let _ = tx.send(
                        res.map(|_| ())
                            .map_err(|e| CoreError::Network(e.to_string())),
                    );
                }
            }
            QueryResult::GetRecord(res) => {
                if let Ok(GetRecordOk::FoundRecord(peer_record)) = &res {
                    if let Some((_, acc)) = self.pending_get_record.get_mut(&id) {
                        acc.push(peer_record.record.value.clone());
                    }
                }
                if last {
                    if let Some((tx, acc)) = self.pending_get_record.remove(&id) {
                        let _ = tx.send(acc);
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_block_message(
        &mut self,
        message: request_response::Message<BlockRequest, BlockResponse>,
    ) {
        match message {
            request_response::Message::Request {
                request, channel, ..
            } => {
                // CHECKPOINT MODÉRATION : ne jamais servir un contenu matché
                // par denylist CID directe (`is_blocked`). Plus de check
                // dérivé "clé bloquée" ici (retiré, finding critique I2) — un
                // CID individuel n'est bloqué que s'il figure explicitement
                // dans une denylist ; une clé bannie n'a de toute façon
                // aucune influence sur ce que ce nœud sert par ailleurs.
                let blocked = self
                    .moderation
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let data = Cid::try_from(request.0.as_slice()).ok().and_then(|cid| {
                    if blocked.is_blocked(&cid) {
                        None
                    } else {
                        self.blockstore.get(&cid).ok()
                    }
                });
                let _ = self
                    .swarm
                    .behaviour_mut()
                    .blocks
                    .send_response(channel, BlockResponse(data));
            }
            request_response::Message::Response {
                request_id,
                response,
            } => {
                if let Some(tx) = self.pending_request.remove(&request_id) {
                    let res = match response.0 {
                        Some(bytes) => Ok(bytes),
                        None => Err(CoreError::BlockNotFound("réponse vide".into())),
                    };
                    let _ = tx.send(res);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blockstore::Blockstore;
    use crate::content::cid_for;

    /// Le point de commit du seed ne retient une publication que si l'émetteur
    /// est toujours abonné ET non banni ; le ban prime sur l'abonnement (garde
    /// déterministe du TOCTOU purge/seed, non forçable en test d'intégration).
    #[test]
    fn seed_commit_requires_subscribed_and_not_banned() {
        assert!(seed_still_wanted(true, false), "abonné, non banni → commit");
        assert!(
            !seed_still_wanted(true, true),
            "abonné MAIS banni par denylist → jamais committé (le ban prime)"
        );
        assert!(
            !seed_still_wanted(false, false),
            "plus abonné (désabonnement / blocage local) → abandon"
        );
        assert!(
            !seed_still_wanted(false, true),
            "ni abonné ni autorisé → abandon"
        );
    }

    async fn spawn_node(dir: &Path, name: &str) -> Node {
        let bs = Blockstore::open(dir.join(name)).unwrap();
        Node::new(Keypair::generate_ed25519(), bs).await.unwrap()
    }

    /// Un pair qui publie des feeds invalides sur le topic gossipsub doit voir
    /// son score chuter (peer scoring) : c'est la base de la réputation qui
    /// finit par graylister les inondeurs de feeds signés par des clés jetables.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn peer_publishing_invalid_feeds_gets_negative_gossip_score() {
        let dir = tempfile::tempdir().unwrap();
        let honest = spawn_node(dir.path(), "honest").await;
        let malicious = spawn_node(dir.path(), "malicious").await;

        let addr = honest
            .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .await
            .unwrap();
        malicious
            .add_address(honest.peer_id(), addr.clone())
            .await
            .unwrap();
        malicious.dial(addr).await.unwrap();

        // Le malveillant inonde le topic d'octets qui ne sont pas des feeds.
        // Chaque message rejeté par la validation applicative doit dégrader son
        // score chez le pair honnête.
        tokio::time::timeout(Duration::from_secs(30), async {
            let mut i: u64 = 0;
            loop {
                i += 1;
                let (tx, rx) = oneshot::channel();
                malicious
                    .cmd_tx
                    .send(Command::PublishFeed {
                        data: format!("pas un feed {i}").into_bytes(),
                        tx,
                    })
                    .await
                    .unwrap();
                let _ = rx.await;
                if honest
                    .gossip_peer_score(malicious.peer_id())
                    .await
                    .unwrap()
                    .is_some_and(|s| s < 0.0)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
        })
        .await
        .expect("le score du pair malveillant doit devenir négatif");
    }

    /// Agrégation par channel (tâche 4, lot d) : deux rapporteurs distincts,
    /// un chacun sur un des deux CIDs du même émetteur → cumul de 2
    /// rapporteurs sur 2 CIDs signalés. Un CID signalé dont l'émetteur est
    /// inconnu du catalogue local reste dans l'agrégat global mais absent du
    /// résultat par channel (limite documentée).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn report_counts_by_channel_joins_reports_with_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let node = spawn_node(dir.path(), "n").await;

        let issuer_kp = Keypair::generate_ed25519();
        let issuer = issuer_kp.public().to_peer_id();
        let cid_a = cid_for(b"report-a");
        let cid_b = cid_for(b"report-b");
        node.apply_feed_for_tests(Feed::build_signed(&issuer_kp, 1, &[cid_a, cid_b]).unwrap())
            .unwrap();

        let reporter1 = Keypair::generate_ed25519();
        let reporter2 = Keypair::generate_ed25519();
        {
            let mut book = node.reports.lock().unwrap();
            book.apply(&Report::build_signed(&reporter1, &cid_a, "denylist").unwrap())
                .unwrap();
            book.apply(&Report::build_signed(&reporter2, &cid_b, "denylist").unwrap())
                .unwrap();
        }

        let by_channel = node.report_counts_by_channel();
        assert_eq!(by_channel.len(), 1);
        assert_eq!(by_channel[0], (issuer, 2, 2));

        // CID hors catalogue : compté globalement, absent du join par channel.
        let unknown_cid = cid_for(b"report-unknown");
        node.reports
            .lock()
            .unwrap()
            .apply(&Report::build_signed(&reporter1, &unknown_cid, "denylist").unwrap())
            .unwrap();

        assert_eq!(node.report_counts_by_channel(), by_channel);
        assert!(node
            .report_counts()
            .iter()
            .any(|(cid, _)| *cid == unknown_cid));
    }

    /// Régression : avec la validation applicative activée (les messages ne
    /// sont plus relayés automatiquement), un feed VALIDE doit toujours être
    /// relayé de proche en proche — A n'est pas connecté à C, seul B peut
    /// transmettre, ce qu'il ne fait que s'il rapporte l'acceptation.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn valid_feed_is_forwarded_across_a_relay_hop() {
        let dir = tempfile::tempdir().unwrap();
        let node_a = spawn_node(dir.path(), "fa").await;
        let node_b = spawn_node(dir.path(), "fb").await;
        let node_c = spawn_node(dir.path(), "fc").await;

        let addr_b = node_b
            .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .await
            .unwrap();
        for node in [&node_a, &node_c] {
            node.add_address(node_b.peer_id(), addr_b.clone())
                .await
                .unwrap();
            node.dial(addr_b.clone()).await.unwrap();
        }

        let cid = cid_for(b"contenu relaye");
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                node_a.publish_feed(&[cid]).await.unwrap();
                if node_c.catalog_cids().contains(&cid) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        })
        .await
        .expect("un feed valide doit traverser le saut de relais gossip");
    }

    /// Un tiers ne doit pas pouvoir écraser le record DHT du feed d'un créateur
    /// avec des octets arbitraires : les nœuds stockeurs valident (signature +
    /// correspondance clé/émetteur) avant de stocker.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stored_feed_record_survives_overwrite_by_third_party() {
        let dir = tempfile::tempdir().unwrap();
        let storer = spawn_node(dir.path(), "s").await;
        let victim = spawn_node(dir.path(), "v").await;
        let attacker = spawn_node(dir.path(), "a").await;

        let addr_s = storer
            .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .await
            .unwrap();
        for node in [&victim, &attacker] {
            node.add_address(storer.peer_id(), addr_s.clone())
                .await
                .unwrap();
            node.dial(addr_s.clone()).await.unwrap();
        }

        let victim_peer = victim.peer_id();
        let published = vec![cid_for(b"contenu du feed de la victime")];

        // La victime publie son feed jusqu'à ce qu'il soit découvrable par
        // l'attaquant (le temps que la table de routage converge).
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                victim.publish_feed(&published).await.unwrap();
                if attacker.fetch_feed(victim_peer).await.unwrap().is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
        })
        .await
        .expect("le feed légitime doit être découvrable avant l'attaque");

        // Attaque : PUT d'octets arbitraires sous la clé du feed de la victime.
        // Sans filtrage des records, les nœuds stockeurs remplacent le record
        // légitime et la découverte du feed est cassée.
        let (tx, rx) = oneshot::channel();
        attacker
            .cmd_tx
            .send(Command::PutRecord {
                key: feed_record_key(&victim_peer),
                value: b"pas un feed valide".to_vec(),
                tx,
            })
            .await
            .unwrap();
        let _ = rx.await;

        // Le feed légitime doit toujours être découvrable après l'attaque.
        let feed = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Some(feed) = attacker.fetch_feed(victim_peer).await.unwrap() {
                    break feed;
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
        })
        .await
        .expect("le feed légitime doit survivre à la tentative d'écrasement");
        assert_eq!(feed.cids().unwrap(), published);
    }

    /// Nouveau contrat (retrait de seed-what-you-consume) : `get` par défaut
    /// (`StorePolicy::Stream`) rend les octets mais n'engage plus le nœud à
    /// seeder — ni cache local, ni provider record.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn get_default_does_not_store_or_provide() {
        let dir = tempfile::tempdir().unwrap();
        let node_a = spawn_node(dir.path(), "sa").await;
        let node_b = spawn_node(dir.path(), "sb").await;

        let addr_a = node_a
            .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .await
            .unwrap();
        node_b
            .add_address(node_a.peer_id(), addr_a.clone())
            .await
            .unwrap();
        node_b.dial(addr_a).await.unwrap();

        let payload = b"contenu simplement streame".to_vec();
        let cid = node_a.add(&payload).await.unwrap();

        let received = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Ok(b) = node_b.get(cid).await {
                    return b;
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
        })
        .await
        .expect("le transfert doit aboutir");
        assert_eq!(received, payload);

        assert!(
            !node_b.blockstore().has(&cid),
            "Stream ne doit pas mettre le bloc en cache"
        );
        // Laisse le temps à une éventuelle (mauvaise) réannonce de se produire.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let providers = node_a.get_providers(cid).await.unwrap();
        assert!(
            !providers.contains(&node_b.peer_id()),
            "Stream ne doit pas faire de B un fournisseur"
        );
    }

    /// `get_with(Seed)` reproduit l'ancien comportement : cache local ET
    /// annonce (le nœud devient fournisseur découvrable).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn get_with_seed_stores_and_provides() {
        let dir = tempfile::tempdir().unwrap();
        let node_a = spawn_node(dir.path(), "sda").await;
        let node_b = spawn_node(dir.path(), "sdb").await;

        let addr_a = node_a
            .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .await
            .unwrap();
        node_b
            .add_address(node_a.peer_id(), addr_a.clone())
            .await
            .unwrap();
        node_b.dial(addr_a).await.unwrap();

        let payload = b"contenu explicitement seede".to_vec();
        let cid = node_a.add(&payload).await.unwrap();

        let received = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Ok(b) = node_b.get_with(cid, StorePolicy::Seed).await {
                    return b;
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
        })
        .await
        .expect("le transfert doit aboutir");
        assert_eq!(received, payload);
        assert!(
            node_b.blockstore().has(&cid),
            "Seed doit mettre le bloc en cache"
        );

        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let providers = node_a.get_providers(cid).await.unwrap();
                if providers.contains(&node_b.peer_id()) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
        })
        .await
        .expect("Seed doit faire de B un fournisseur découvrable");
    }

    /// Le checkpoint modération #2 s'applique dans les deux politiques : un
    /// CID matché est refusé sans jamais être stocké, que la politique
    /// demandée soit `Stream` ou `Seed`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn moderation_checkpoint_applies_in_both_policies() {
        let dir = tempfile::tempdir().unwrap();
        let forbidden = b"contenu interdit, seed ou stream".to_vec();
        let bad_cid = cid_for(&forbidden);

        let kp_a = Keypair::generate_ed25519();
        let bs_a = Blockstore::open(dir.path().join("ma")).unwrap();
        let node_a = Node::new(kp_a, bs_a).await.unwrap();
        let addr_a = node_a
            .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .await
            .unwrap();
        node_a.add(&forbidden).await.unwrap();

        let issuer = identity::load_or_generate(dir.path().join("issuer.key")).unwrap();
        let dl = Denylist::build_signed("test", "2026-06-24T00:00:00Z", &issuer, &[bad_cid], &[])
            .unwrap();
        let mut moderation = Moderation::empty();
        moderation.subscribe(&dl).unwrap();

        for (name, policy) in [
            ("mb-stream", StorePolicy::Stream),
            ("mb-seed", StorePolicy::Seed),
        ] {
            let kp_b = Keypair::generate_ed25519();
            let bs_b = Blockstore::open(dir.path().join(name)).unwrap();
            let node_b = Node::with_moderation(kp_b, bs_b, moderation.clone())
                .await
                .unwrap();
            node_b
                .add_address(node_a.peer_id(), addr_a.clone())
                .await
                .unwrap();
            node_b.dial(addr_a.clone()).await.unwrap();

            let err = tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    match node_b.get_with(bad_cid, policy).await {
                        Err(CoreError::Moderated(_)) => return true,
                        Err(CoreError::NoProviders(_)) | Err(CoreError::BlockNotFound(_)) => {
                            tokio::time::sleep(Duration::from_millis(300)).await;
                        }
                        other => panic!("résultat inattendu: {other:?}"),
                    }
                }
            })
            .await
            .expect("le refus doit survenir");
            assert!(err, "le checkpoint #2 doit refuser {policy:?}");
            assert!(
                !node_b.blockstore().has(&bad_cid),
                "aucun stockage sous {policy:?}"
            );
        }
    }
}
