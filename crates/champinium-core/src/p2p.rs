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
use crate::catalog::{Catalog, CatalogEntry};
use crate::content::{cid_for, verify};
use crate::error::{CoreError, Result as CoreResult};
use crate::feed::Feed;
use crate::identity;
use crate::ingest::{self, HlsManifest, HlsSegment};
use crate::moderation::{Denylist, Moderation};
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
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

const BLOCK_PROTOCOL: &str = "/champinium/block/1.0.0";
const IDENTIFY_PROTOCOL: &str = "/champinium/0.1.0";
const FEEDS_TOPIC: &str = "champinium/feeds/v1";

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
        // Feeds signés diffusés en gossipsub. Messages signés par l'identité libp2p.
        let gossipsub_cfg = gossipsub::ConfigBuilder::default()
            .max_transmit_size(MAX_FEED_SIZE)
            .build()
            .expect("config gossipsub valide");
        let gossipsub = gossipsub::Behaviour::new(
            gossipsub::MessageAuthenticity::Signed(key.clone()),
            gossipsub_cfg,
        )
        .expect("config gossipsub valide");
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
        cid: Cid,
        tx: oneshot::Sender<CoreResult<()>>,
    },
    GetProviders {
        cid: Cid,
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
}

/// Poignée vers un nœud P2P en fonctionnement.
#[derive(Clone)]
pub struct Node {
    peer_id: PeerId,
    keypair: Keypair,
    blockstore: Blockstore,
    moderation: Arc<RwLock<Moderation>>,
    catalog: Arc<Mutex<Catalog>>,
    feed_seq: Arc<Mutex<u64>>,
    cmd_tx: mpsc::Sender<Command>,
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
        let peer_id = identity::peer_id(&keypair);
        let mut swarm = build_swarm(keypair.clone())?;
        // Mode serveur : stocke et sert les provider records (pas seulement client).
        swarm
            .behaviour_mut()
            .kademlia
            .set_mode(Some(kad::Mode::Server));
        // Souscription au topic des feeds.
        let topic = gossipsub::IdentTopic::new(FEEDS_TOPIC);
        swarm
            .behaviour_mut()
            .gossipsub
            .subscribe(&topic)
            .map_err(|e| CoreError::Network(format!("gossipsub subscribe: {e}")))?;

        let moderation = Arc::new(RwLock::new(moderation));
        let catalog = Arc::new(Mutex::new(Catalog::new()));
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let event_loop = EventLoop::new(
            swarm,
            blockstore.clone(),
            moderation.clone(),
            catalog.clone(),
            topic,
            cmd_rx,
        );
        tokio::spawn(event_loop.run());

        // Reprend le seq de feed là où il s'était arrêté (sinon un nœud redémarré
        // republierait un seq plus petit, ignoré par le LWW des catalogues pairs).
        let feed_seq = Arc::new(Mutex::new(load_feed_seq(&blockstore)));

        Ok(Self {
            peer_id,
            keypair,
            blockstore,
            moderation,
            catalog,
            feed_seq,
            cmd_tx,
        })
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
    /// ses CIDs au moteur, puis **purge** du magasin local tout bloc désormais
    /// interdit (checkpoint rétroactif). Renvoie le nombre de blocs purgés.
    pub async fn subscribe_denylist(&self, list: &Denylist) -> CoreResult<usize> {
        {
            let mut mod_guard = self
                .moderation
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            mod_guard.subscribe(list)?;
        }
        // Purge des blocs déjà stockés que la nouvelle liste couvre.
        let mut purged = 0;
        for cid in self.blockstore.list()? {
            if self.is_blocked(&cid) {
                self.blockstore.remove(&cid)?;
                purged += 1;
            }
        }
        Ok(purged)
    }

    /// Publie un feed signé listant `cids` : l'ajoute au catalogue local puis le
    /// diffuse en gossipsub. Le `seq` est incrémenté à chaque appel.
    pub async fn publish_feed(&self, cids: &[Cid]) -> CoreResult<()> {
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
        let feed = Feed::build_signed(&self.keypair, seq, cids)?;
        // Le créateur figure dans son propre catalogue.
        self.catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .apply(feed.clone())?;
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
        // Diffusion live en gossipsub.
        let (tx, rx) = oneshot::channel();
        self.send(Command::PublishFeed { data, tx }).await?;
        rx.await.map_err(|_| CoreError::Shutdown)?
    }

    /// Récupère le feed d'un créateur depuis la DHT (découverte hors gossip).
    /// Vérifie la signature et l'émetteur, retient le `seq` le plus élevé, et
    /// l'applique au catalogue local. Renvoie `None` si aucun feed valide trouvé.
    pub async fn fetch_feed(&self, issuer: PeerId) -> CoreResult<Option<Feed>> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::GetRecord {
            key: feed_record_key(&issuer),
            tx,
        })
        .await?;
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
        if let Some(feed) = &best {
            let _ = self
                .catalog
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .apply(feed.clone());
        }
        Ok(best)
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
        for (path, duration) in segs {
            let bytes = tokio::fs::read(&path).await?;
            let cid = self.add(&bytes).await?; // modération #1 + store + provide
            segments.push(HlsSegment {
                cid: cid.to_string(),
                duration,
            });
        }
        let manifest = HlsManifest::new(target, segments);
        self.add(manifest.to_json()?.as_bytes()).await
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
        let bytes = self.get(manifest_cid).await?;
        let manifest = HlsManifest::from_json(&bytes)?;
        tokio::fs::create_dir_all(out_dir).await?;
        for seg in &manifest.segments {
            let cid: Cid = seg.cid.parse().map_err(CoreError::Cid)?;
            let data = self.get(cid).await?;
            tokio::fs::write(out_dir.join(format!("{}.ts", seg.cid)), &data).await?;
        }
        let playlist = out_dir.join("index.m3u8");
        tokio::fs::write(&playlist, manifest.to_m3u8()).await?;
        Ok(playlist)
    }

    /// Instantané des entrées du catalogue reconstruit (un feed par émetteur).
    pub fn catalog_entries(&self) -> Vec<CatalogEntry> {
        self.catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries()
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

    /// Annonce que ce nœud fournit `cid`. **Ne fait rien** si le CID est bloqué :
    /// s'annoncer fournisseur d'un contenu qu'on refuse de servir serait à la
    /// fois incohérent et un signal au réseau qu'on le détient.
    pub async fn provide(&self, cid: Cid) -> CoreResult<()> {
        if self.is_blocked(&cid) {
            return Ok(());
        }
        let (tx, rx) = oneshot::channel();
        self.send(Command::Provide { cid, tx }).await?;
        rx.await.map_err(|_| CoreError::Shutdown)?
    }

    /// Recherche les fournisseurs d'un CID via la DHT.
    pub async fn get_providers(&self, cid: Cid) -> CoreResult<HashSet<PeerId>> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::GetProviders { cid, tx }).await?;
        rx.await.map_err(|_| CoreError::Shutdown)
    }

    /// Récupère un bloc : cache local → sinon découverte DHT + transfert + vérif.
    /// Interroge **tous les fournisseurs en parallèle** et retient la première
    /// réponse valide. Applique seed-what-you-consume : le bloc est mis en cache
    /// **et réannoncé** (le consommateur devient fournisseur → réplication).
    ///
    /// CHECKPOINT MODÉRATION #2 (réception) : un contenu matché n'est ni récupéré,
    /// ni mis en cache, ni reseedé.
    pub async fn get(&self, cid: Cid) -> CoreResult<Vec<u8>> {
        if self.is_blocked(&cid) {
            return Err(CoreError::Moderated(cid.to_string()));
        }
        if self.blockstore.has(&cid) {
            match self.blockstore.get(&cid) {
                Ok(bytes) => return Ok(bytes),
                // Cache local corrompu (ex. crash pendant l'écriture) : on
                // retombe sur le réseau au lieu de rendre le CID irrécupérable ;
                // le `put` en fin de fetch réparera le fichier.
                Err(CoreError::IntegrityMismatch) => {
                    tracing::warn!("bloc local corrompu, re-téléchargement: {cid}");
                }
                Err(e) => return Err(e),
            }
        }
        let providers = self.get_providers(cid).await?;
        if providers.is_empty() {
            return Err(CoreError::NoProviders(cid.to_string()));
        }

        // Requêtes concurrentes vers tous les fournisseurs ; première réponse
        // valide (CID vérifié) gagne, les autres sont abandonnées. On s'exclut
        // soi-même (on peut être fournisseur annoncé d'un bloc local corrompu).
        let mut inflight: FuturesUnordered<_> = providers
            .into_iter()
            .filter(|peer| *peer != self.peer_id)
            .map(|peer| self.request_block(peer, cid))
            .collect();
        // `request_block` a déjà vérifié le CID : la première réponse OK est bonne.
        while let Some(result) = inflight.next().await {
            match result {
                Ok(bytes) => {
                    self.blockstore.put(&bytes)?;
                    // seed-what-you-consume : devient fournisseur (best-effort).
                    let _ = self.provide(cid).await;
                    return Ok(bytes);
                }
                Err(e) => tracing::debug!("fournisseur rejeté pour {cid}: {e}"),
            }
        }
        Err(CoreError::BlockNotFound(cid.to_string()))
    }

    /// Demande un bloc précis à un pair précis. Les octets reçus sont **vérifiés
    /// contre le CID** avant d'être renvoyés : un pair ne peut pas faire passer un
    /// contenu arbitraire (tout appelant, pas seulement `get`, est protégé).
    pub async fn request_block(&self, peer: PeerId, cid: Cid) -> CoreResult<Vec<u8>> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::RequestBlock { peer, cid, tx }).await?;
        let bytes = rx.await.map_err(|_| CoreError::Shutdown)??;
        if !verify(&cid, &bytes) {
            return Err(CoreError::IntegrityMismatch);
        }
        Ok(bytes)
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

/// Préfixe des clés DHT de feeds.
const FEED_KEY_PREFIX: &[u8] = b"/champinium/feed/";

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
    feeds_topic: gossipsub::IdentTopic,
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
    fn new(
        swarm: Swarm<Behaviour>,
        blockstore: Blockstore,
        moderation: Arc<RwLock<Moderation>>,
        catalog: Arc<Mutex<Catalog>>,
        feeds_topic: gossipsub::IdentTopic,
        cmd_rx: mpsc::Receiver<Command>,
    ) -> Self {
        Self {
            swarm,
            blockstore,
            moderation,
            catalog,
            feeds_topic,
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
            Command::Provide { cid, tx } => {
                let key = RecordKey::new(&cid.to_bytes());
                match self.swarm.behaviour_mut().kademlia.start_providing(key) {
                    Ok(qid) => {
                        self.pending_provide.insert(qid, tx);
                    }
                    Err(e) => {
                        let _ = tx.send(Err(CoreError::Network(e.to_string())));
                    }
                }
            }
            Command::GetProviders { cid, tx } => {
                let key = RecordKey::new(&cid.to_bytes());
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
                message,
                ..
            })) => self.handle_feed_message(&message.data),
            _ => {}
        }
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
    /// dans `Catalog::apply`). Les feeds invalides sont ignorés silencieusement.
    fn handle_feed_message(&self, data: &[u8]) {
        match Feed::from_json(data) {
            Ok(feed) => {
                if let Err(e) = self
                    .catalog
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .apply(feed)
                {
                    tracing::debug!("feed rejeté: {e}");
                }
            }
            Err(e) => tracing::debug!("feed illisible: {e}"),
        }
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
                // CHECKPOINT MODÉRATION : ne jamais servir un contenu matché.
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

    async fn spawn_node(dir: &Path, name: &str) -> Node {
        let bs = Blockstore::open(dir.join(name)).unwrap();
        Node::new(Keypair::generate_ed25519(), bs).await.unwrap()
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
}
