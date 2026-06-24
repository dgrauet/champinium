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
use crate::content::{cid_for, verify};
use crate::error::{CoreError, Result as CoreResult};
use crate::identity;
use crate::moderation::Moderation;
use cid::Cid;
use futures::StreamExt;
use libp2p::kad::{self, store::MemoryStore, GetProvidersOk, QueryId, QueryResult, RecordKey};
use libp2p::request_response::{self, OutboundRequestId, ProtocolSupport};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{identify, identity::Keypair, noise, ping, tcp, yamux};
use libp2p::{Multiaddr, PeerId, StreamProtocol, Swarm};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

const BLOCK_PROTOCOL: &str = "/champinium/block/1.0.0";
const IDENTIFY_PROTOCOL: &str = "/champinium/0.1.0";

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
}

impl Behaviour {
    fn new(key: &Keypair) -> Self {
        let peer_id = key.public().to_peer_id();
        let kademlia = kad::Behaviour::new(peer_id, MemoryStore::new(peer_id));
        let identify = identify::Behaviour::new(identify::Config::new(
            IDENTIFY_PROTOCOL.to_string(),
            key.public(),
        ));
        let blocks = request_response::cbor::Behaviour::new(
            [(StreamProtocol::new(BLOCK_PROTOCOL), ProtocolSupport::Full)],
            request_response::Config::default(),
        );
        Self {
            kademlia,
            identify,
            ping: ping::Behaviour::default(),
            blocks,
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
}

/// Poignée vers un nœud P2P en fonctionnement.
#[derive(Clone)]
pub struct Node {
    peer_id: PeerId,
    blockstore: Blockstore,
    moderation: Arc<Moderation>,
    cmd_tx: mpsc::Sender<Command>,
}

impl Node {
    /// Construit un nœud avec la modération par défaut active (non désactivable).
    pub async fn new(keypair: Keypair, blockstore: Blockstore) -> CoreResult<Self> {
        Self::with_moderation(keypair, blockstore, Moderation::with_default()?).await
    }

    /// Construit un nœud avec un moteur de modération explicite (tests, opérateurs
    /// qui souscrivent à des denylists tierces). Démarre la boucle d'évènements.
    pub async fn with_moderation(
        keypair: Keypair,
        blockstore: Blockstore,
        moderation: Moderation,
    ) -> CoreResult<Self> {
        let peer_id = identity::peer_id(&keypair);
        let mut swarm = build_swarm(keypair)?;
        // Mode serveur : stocke et sert les provider records (pas seulement client).
        swarm
            .behaviour_mut()
            .kademlia
            .set_mode(Some(kad::Mode::Server));

        let moderation = Arc::new(moderation);
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let event_loop = EventLoop::new(swarm, blockstore.clone(), moderation.clone(), cmd_rx);
        tokio::spawn(event_loop.run());

        Ok(Self {
            peer_id,
            blockstore,
            moderation,
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

    /// Accès au moteur de modération.
    pub fn moderation(&self) -> &Moderation {
        &self.moderation
    }

    /// Stocke un bloc localement et l'annonce dans la DHT (provider record).
    ///
    /// CHECKPOINT MODÉRATION #1 (ingestion) : un contenu matché est refusé — ni
    /// stocké, ni annoncé.
    pub async fn add(&self, bytes: &[u8]) -> CoreResult<Cid> {
        let cid = cid_for(bytes);
        if self.moderation.is_blocked(&cid) {
            return Err(CoreError::Moderated(cid.to_string()));
        }
        let cid = self.blockstore.put(bytes)?;
        self.provide(cid).await?;
        Ok(cid)
    }

    /// Annonce que ce nœud fournit `cid`.
    pub async fn provide(&self, cid: Cid) -> CoreResult<()> {
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
    /// Applique seed-what-you-consume (le bloc récupéré est remis en cache).
    ///
    /// CHECKPOINT MODÉRATION #2 (réception) : un contenu matché n'est ni récupéré,
    /// ni mis en cache, ni reseedé.
    pub async fn get(&self, cid: Cid) -> CoreResult<Vec<u8>> {
        if self.moderation.is_blocked(&cid) {
            return Err(CoreError::Moderated(cid.to_string()));
        }
        if self.blockstore.has(&cid) {
            return self.blockstore.get(&cid);
        }
        let providers = self.get_providers(cid).await?;
        if providers.is_empty() {
            return Err(CoreError::NoProviders(cid.to_string()));
        }
        for peer in providers {
            if let Ok(bytes) = self.request_block(peer, cid).await {
                if verify(&cid, &bytes) {
                    self.blockstore.put(&bytes)?; // seed-what-you-consume
                    return Ok(bytes);
                }
            }
        }
        Err(CoreError::BlockNotFound(cid.to_string()))
    }

    /// Demande un bloc précis à un pair précis.
    pub async fn request_block(&self, peer: PeerId, cid: Cid) -> CoreResult<Vec<u8>> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::RequestBlock { peer, cid, tx }).await?;
        rx.await.map_err(|_| CoreError::Shutdown)?
    }

    async fn send(&self, cmd: Command) -> CoreResult<()> {
        self.cmd_tx.send(cmd).await.map_err(|_| CoreError::Shutdown)
    }
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
        .with_behaviour(|key| {
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(Behaviour::new(key))
        })
        .map_err(|e| CoreError::Network(e.to_string()))?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();
    Ok(swarm)
}

struct EventLoop {
    swarm: Swarm<Behaviour>,
    blockstore: Blockstore,
    moderation: Arc<Moderation>,
    cmd_rx: mpsc::Receiver<Command>,
    pending_listen: VecDeque<oneshot::Sender<CoreResult<Multiaddr>>>,
    pending_provide: HashMap<QueryId, oneshot::Sender<CoreResult<()>>>,
    pending_get_providers: HashMap<QueryId, (oneshot::Sender<HashSet<PeerId>>, HashSet<PeerId>)>,
    pending_request: HashMap<OutboundRequestId, oneshot::Sender<CoreResult<Vec<u8>>>>,
}

impl EventLoop {
    fn new(
        swarm: Swarm<Behaviour>,
        blockstore: Blockstore,
        moderation: Arc<Moderation>,
        cmd_rx: mpsc::Receiver<Command>,
    ) -> Self {
        Self {
            swarm,
            blockstore,
            moderation,
            cmd_rx,
            pending_listen: VecDeque::new(),
            pending_provide: HashMap::new(),
            pending_get_providers: HashMap::new(),
            pending_request: HashMap::new(),
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
                Ok(_) => self.pending_listen.push_back(tx),
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
        }
    }

    fn handle_event(&mut self, event: SwarmEvent<BehaviourEvent>) {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                if let Some(tx) = self.pending_listen.pop_front() {
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
            _ => {}
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
                let data = Cid::try_from(request.0.as_slice()).ok().and_then(|cid| {
                    if self.moderation.is_blocked(&cid) {
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
