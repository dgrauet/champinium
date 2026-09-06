//! Surface UniFFI exposée aux fronts natifs (contrat v9).
//!
//! C'est la **frontière de contrat** : les fronts (Swift, C#) codent contre cet
//! objet, jamais contre l'implémentation. Toute évolution passe par l'agent NOYAU
//! et incrémente `CONTRACT_VERSION` (voir `/AGENTS.md`).
//!
//! Le risque #1 (async via FFI) est éprouvé ici : la plupart des méthodes sont
//! `async` (runtime tokio) et exposées vers Swift ET C#.

use crate::Node;
use cid::Cid;
use std::path::PathBuf;
use std::sync::Arc;

/// Erreur **typée** exposée aux fronts : chaque variante appelle une UX
/// différente (un contenu bloqué par la modération n'est pas une panne réseau).
/// Le message conserve le diagnostic détaillé du noyau.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiError {
    /// Contenu refusé par la modération (denylist) — à présenter comme un
    /// blocage volontaire, pas comme une erreur technique.
    #[error("contenu refusé par la modération: {msg}")]
    Moderated { msg: String },
    /// Erreur réseau / P2P (connexion, transfert, DHT).
    #[error("réseau: {msg}")]
    Network { msg: String },
    /// Contenu introuvable (aucun fournisseur, bloc absent).
    #[error("introuvable: {msg}")]
    NotFound { msg: String },
    /// Entrée fournie par le front invalide (CID, multiaddr, JSON…).
    #[error("entrée invalide: {msg}")]
    InvalidInput { msg: String },
    /// Erreur interne du noyau (I/O, ingestion, identité, arrêt).
    #[error("interne: {msg}")]
    Internal { msg: String },
}

impl From<crate::CoreError> for FfiError {
    fn from(e: crate::CoreError) -> Self {
        use crate::CoreError as E;
        let msg = e.to_string();
        match e {
            E::Moderated(_) => FfiError::Moderated { msg },
            // L'intégrité échoue sur des octets reçus du réseau : côté front,
            // c'est un incident de transfert, pas une entrée invalide.
            E::Network(_) | E::IntegrityMismatch => FfiError::Network { msg },
            E::NoProviders(_) | E::BlockNotFound(_) => FfiError::NotFound { msg },
            E::Cid(_) | E::Moderation(_) => FfiError::InvalidInput { msg },
            E::Io(_) | E::Identity(_) | E::Ingest(_) | E::Shutdown => FfiError::Internal { msg },
        }
    }
}

/// Callback implémenté par les fronts : rappelé (depuis un thread du runtime
/// tokio, PAS le thread UI) à chaque changement effectif du catalogue. Le front
/// doit re-dispatcher vers son thread principal puis relire `catalog()` —
/// l'événement est un simple tic fusionnable, il ne porte pas les données.
#[uniffi::export(with_foreign)]
pub trait CatalogListener: Send + Sync {
    /// Le catalogue local a changé (feed gossip, publication locale, feed DHT).
    fn on_catalog_updated(&self);
}

/// Callback implémenté par les fronts : rappelé à chaque changement effectif du
/// seed proactif (publication nouvellement seedée ou purgée). Même contrat que
/// `CatalogListener` : tic fusionnable, le front re-dispatche puis relit
/// `storage_stats()` / `catalog()`.
#[uniffi::export(with_foreign)]
pub trait SeedListener: Send + Sync {
    /// L'état du seed proactif a changé (couverture, quota, pins).
    fn on_seed_updated(&self);
}

/// Callback implémenté par les fronts : rappelé à chaque changement d'état
/// d'une session de lecture progressive (segment reçu, échec, fermeture).
/// Même contrat que `SeedListener` : tic fusionnable, le front re-dispatche
/// puis relit `stream_status(id)`.
#[uniffi::export(with_foreign)]
pub trait StreamListener: Send + Sync {
    fn on_stream_updated(&self, id: u64);
}

/// Callback implémenté par les fronts : rappelé à chaque changement effectif
/// de la modération (souscription/désabonnement d'un éditeur de denylist,
/// liste rafraîchie). Même contrat que `SeedListener` : tic fusionnable, le
/// front re-dispatche puis relit `denylist_sources()`.
#[uniffi::export(with_foreign)]
pub trait ModerationListener: Send + Sync {
    fn on_moderation_updated(&self);
}

/// Instantané des entrées connues pour un éditeur de denylist souscrit
/// (contrat v13, ADR 0011). `locked = true` pour l'éditeur projet (denylist
/// par défaut, désabonnement refusé). `fetched = false` tant qu'aucune liste
/// n'a encore été récupérée pour cet éditeur : les autres champs sont alors
/// vides/zéro plutôt que d'inventer une valeur.
#[derive(uniffi::Record)]
pub struct FfiDenylistSource {
    pub peer_id: String,
    pub name: String,
    pub seq: u64,
    pub entry_count: u32,
    pub key_count: u32,
    pub updated: String,
    pub locked: bool,
    pub fetched: bool,
}

/// Session de lecture ouverte : `url` se donne telle quelle au lecteur natif
/// (`http://127.0.0.1:<port>/<jeton>/index.m3u8`), jamais parsée côté front.
#[derive(uniffi::Record)]
pub struct FfiStreamSession {
    pub id: u64,
    pub url: String,
    pub total_segments: u32,
}

/// État d'une session : progression et, le cas échéant, échec définitif
/// (contenu refusé par la modération).
#[derive(uniffi::Record)]
pub struct FfiStreamStatus {
    pub fetched_segments: u32,
    pub total_segments: u32,
    pub failed_reason: Option<String>,
}

/// Mode de provenance **déclaré** par le créateur (feed v4, ADR 0010) — une
/// affirmation signée, pas une vérification. `Undeclared` est explicite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiProvenanceMode {
    Generated,
    Assisted,
    Captured,
    Undeclared,
}

/// Déclaration de provenance d'un contenu : mode + outils (normalisés,
/// cherchables comme les tags).
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiProvenance {
    pub mode: FfiProvenanceMode,
    pub tools: Vec<String>,
}

impl From<crate::feed::Provenance> for FfiProvenance {
    fn from(p: crate::feed::Provenance) -> Self {
        use crate::feed::ProvenanceMode as M;
        Self {
            mode: match p.mode {
                M::Generated => FfiProvenanceMode::Generated,
                M::Assisted => FfiProvenanceMode::Assisted,
                M::Captured => FfiProvenanceMode::Captured,
                M::Undeclared => FfiProvenanceMode::Undeclared,
            },
            tools: p.tools,
        }
    }
}

impl From<FfiProvenance> for crate::feed::Provenance {
    fn from(p: FfiProvenance) -> Self {
        use crate::feed::ProvenanceMode as M;
        Self {
            mode: match p.mode {
                FfiProvenanceMode::Generated => M::Generated,
                FfiProvenanceMode::Assisted => M::Assisted,
                FfiProvenanceMode::Captured => M::Captured,
                FfiProvenanceMode::Undeclared => M::Undeclared,
            },
            tools: p.tools,
        }
    }
}

/// Un contenu avec ses métadonnées signées (titre, tags, provenance). Sert à
/// la fois de sortie (catalogue, recherche) et d'entrée (`publish_feed_with`).
#[derive(uniffi::Record)]
pub struct FfiContentItem {
    /// CID du contenu (chaîne CIDv1) — pour une vidéo, le manifeste HLS.
    pub cid: String,
    /// Titre lisible (peut être vide).
    pub title: String,
    /// Tags (normalisés en minuscules à la publication).
    pub tags: Vec<String>,
    /// Déclaration de provenance signée (feed v4).
    pub provenance: FfiProvenance,
}

/// Un résultat de recherche.
#[derive(uniffi::Record)]
pub struct FfiSearchHit {
    /// PeerId du créateur (chaîne).
    pub issuer: String,
    /// CID du contenu.
    pub cid: String,
    /// Titre.
    pub title: String,
    /// Tags.
    pub tags: Vec<String>,
    /// Déclaration de provenance signée (feed v4).
    pub provenance: FfiProvenance,
}

/// Identité éditoriale du channel de ce nœud (spec channels §1).
#[derive(uniffi::Record)]
pub struct FfiChannelProfile {
    pub name: String,
    pub description: String,
    pub avatar_cid: Option<String>,
}

/// Aperçu d'un channel résolu par lien ou PeerId nu (spec 2026-07-23, partie
/// A), voir [`ChampiniumNode::resolve_channel`]. `name`/`description`/
/// `avatar_cid` sont l'identité éditoriale du channel (`ChannelMeta` aplatie —
/// pas de type imbriqué, même choix que `FfiCatalogEntry`).
#[derive(uniffi::Record)]
pub struct FfiChannelPreview {
    /// PeerId de l'émetteur (chaîne).
    pub peer_id: String,
    pub name: String,
    pub description: String,
    pub avatar_cid: Option<String>,
    /// Contenus publiés par ce channel, tels que connus localement.
    pub items: Vec<FfiContentItem>,
    /// Vrai si CE nœud est déjà abonné à cet émetteur.
    pub subscribed: bool,
    /// Vrai si CE nœud a bloqué localement cet émetteur (l'aperçu reste
    /// consultable malgré le blocage — voir doc de `resolve_channel`).
    pub blocked: bool,
}

/// Une entrée de catalogue, vue par les fronts.
#[derive(uniffi::Record)]
pub struct FfiCatalogEntry {
    /// PeerId du créateur (chaîne).
    pub issuer: String,
    /// Version du feed.
    pub seq: u64,
    /// CIDs publiés (chaînes).
    pub cids: Vec<String>,
    /// Contenus enrichis (mêmes CIDs, avec titre/tags signés).
    pub items: Vec<FfiContentItem>,
    /// Identité éditoriale du channel émetteur (signée avec le feed).
    pub channel: FfiChannelProfile,
    /// Nombre de publications de ce feed actuellement seedées par CE nœud
    /// (couverture du seed proactif, lot c).
    pub seeded_count: u32,
    /// Nombre total de publications de ce feed, tel que connu du catalogue local.
    pub total_count: u32,
    /// Manifestes de ce feed épinglés par CE nœud (chaînes CID).
    pub pinned: Vec<String>,
}

/// `(octets_utilisés, quota_octets)` du seed proactif de ce nœud.
#[derive(uniffi::Record)]
pub struct FfiStorageStats {
    pub used_bytes: u64,
    pub quota_bytes: u64,
}

/// Nœud Champinium exposé aux fronts. Encapsule le noyau ; les fronts ne font que
/// de la présentation au-dessus.
#[derive(uniffi::Object)]
pub struct ChampiniumNode {
    inner: Node,
}

/// Ouvre (ou crée) un nœud sous `data_dir` : identité persistée + blocs, avec le
/// moteur de modération actif par défaut (non désactivable) — l'éditeur de la
/// liste projet est souscrit d'office (ADR 0011) ; sa liste signée doit encore
/// être récupérée depuis le réseau pour être appliquée (état « jamais
/// récupérée » tant qu'aucune n'est en cache).
#[uniffi::export(async_runtime = "tokio")]
pub async fn open_node(data_dir: String) -> Result<Arc<ChampiniumNode>, FfiError> {
    let inner = Node::open(&PathBuf::from(data_dir)).await?;
    // Backend de repli froid par défaut (ADR 0008, décision B tâche CS-b 1) :
    // gateways codées en dur, aucune config UI. Aucun effet sans la feature
    // `cold-storage` (le champ `cold` n'existe même pas dans la struct).
    #[cfg(feature = "cold-storage")]
    let inner = inner.with_cold_store(std::sync::Arc::new(
        crate::coldstore::arweave::ArweaveColdStore::new(vec![
            "https://arweave.net".to_string(),
            "https://ar-io.net".to_string(),
        ]),
    ));
    Ok(Arc::new(ChampiniumNode { inner }))
}

/// Répertoire de données durable par défaut selon l'OS (identité + blocs). Les
/// fronts DOIVENT l'utiliser plutôt qu'un répertoire temporaire (sinon perte du
/// PeerId et régression du `seq` au nettoyage du tmp).
#[uniffi::export]
pub fn default_data_dir() -> String {
    crate::paths::default_data_dir()
        .to_string_lossy()
        .into_owned()
}

#[uniffi::export]
impl ChampiniumNode {
    /// PeerId du nœud.
    pub fn peer_id(&self) -> String {
        self.inner.peer_id().to_string()
    }

    /// Catalogue reconstruit (instantané).
    pub fn catalog(&self) -> Vec<FfiCatalogEntry> {
        self.inner
            .catalog_entries()
            .into_iter()
            .map(|e| catalog_entry_to_ffi(&self.inner, e))
            .collect()
    }

    /// Quota de seed courant (octets). Raccourci sync sur `storage_stats()`.
    pub fn seed_quota(&self) -> u64 {
        self.inner.storage_stats().1
    }

    /// `(octets_utilisés, quota_octets)` du seed proactif de ce nœud.
    pub fn storage_stats(&self) -> FfiStorageStats {
        let (used_bytes, quota_bytes) = self.inner.storage_stats();
        FfiStorageStats {
            used_bytes,
            quota_bytes,
        }
    }

    /// Abonnements courants (PeerIds triés) — état local privé, jamais publié.
    pub fn subscriptions(&self) -> Vec<String> {
        self.inner
            .subscriptions()
            .into_iter()
            .map(|p| p.to_string())
            .collect()
    }

    /// Catalogue restreint aux émetteurs souscrits.
    pub fn catalog_subscribed(&self) -> Vec<FfiCatalogEntry> {
        self.inner
            .catalog_subscribed()
            .into_iter()
            .map(|e| catalog_entry_to_ffi(&self.inner, e))
            .collect()
    }

    /// Channels bloqués localement (PeerIds triés) — préférence privée de ce
    /// nœud, jamais publiée (contrat v8, tâche 3).
    pub fn blocked_channels(&self) -> Vec<String> {
        self.inner
            .blocked_channels()
            .into_iter()
            .map(|p| p.to_string())
            .collect()
    }

    /// Lien partageable du channel de `peer_id` (bouton « copier le lien de
    /// mon channel »).
    pub fn channel_link(&self, peer_id: String) -> Result<String, FfiError> {
        let peer = parse_peer_id(&peer_id)?;
        Ok(crate::channel_link::format(&peer))
    }

    /// Lien partageable d'un éditeur de denylist (bouton « copier le lien de
    /// cette liste »).
    pub fn denylist_link(&self, peer_id: String) -> Result<String, FfiError> {
        let peer = parse_peer_id(&peer_id)?;
        Ok(crate::channel_link::denylist_link(&peer))
    }

    /// Éditeurs de denylist souscrits (éditeur projet en premier, verrouillé)
    /// avec leur dernier instantané connu (contrat v13, ADR 0011).
    pub fn denylist_sources(&self) -> Vec<FfiDenylistSource> {
        self.inner
            .denylist_issuers()
            .into_iter()
            .map(|issuer| {
                let locked = Some(issuer) == self.inner.project_issuer();
                match self.inner.denylist_source(&issuer) {
                    Some(src) => FfiDenylistSource {
                        peer_id: issuer.to_string(),
                        name: src.name,
                        seq: src.seq,
                        entry_count: src.entry_count as u32,
                        key_count: src.key_count as u32,
                        updated: src.updated,
                        locked,
                        fetched: true,
                    },
                    None => FfiDenylistSource {
                        peer_id: issuer.to_string(),
                        name: String::new(),
                        seq: 0,
                        entry_count: 0,
                        key_count: 0,
                        updated: String::new(),
                        locked,
                        fetched: false,
                    },
                }
            })
            .collect()
    }

    /// Recherche **locale** (titres et tags du catalogue reconstruit). Limite
    /// assumée : ne couvre que ce que ce nœud a vu passer.
    pub fn search(&self, query: String) -> Vec<FfiSearchHit> {
        self.inner
            .search(&query)
            .into_iter()
            .map(search_hit_to_ffi)
            .collect()
    }

    /// Profil de channel courant de CE nœud.
    pub fn channel_profile(&self) -> FfiChannelProfile {
        let m = self.inner.channel_profile();
        FfiChannelProfile {
            name: m.name,
            description: m.description,
            avatar_cid: m.avatar_cid,
        }
    }

    /// Le repli de récupération froide est-il actif ? (persisté, actif par défaut)
    pub fn cold_retrieval_enabled(&self) -> bool {
        self.inner.cold_retrieval_enabled()
    }

    /// Active/désactive le repli de récupération froide (persisté). Sans effet
    /// tant que le binaire n'est pas compilé avec la feature `cold-storage`,
    /// mais le choix est mémorisé quoi qu'il arrive.
    pub fn set_cold_retrieval(&self, enabled: bool) -> Result<(), FfiError> {
        self.inner.set_cold_retrieval(enabled)?;
        Ok(())
    }

    /// État d'une session de lecture ; id inconnu → `NotFound`.
    pub fn stream_status(&self, id: u64) -> Result<FfiStreamStatus, FfiError> {
        let s = self.inner.stream_status(id)?;
        Ok(FfiStreamStatus {
            fetched_segments: s.fetched_segments,
            total_segments: s.total_segments,
            failed_reason: s.failed_reason,
        })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl ChampiniumNode {
    /// Écoute sur `addr` (multiaddr) ; renvoie l'adresse effectivement liée.
    pub async fn listen(&self, addr: String) -> Result<String, FfiError> {
        let addr = addr.parse().map_err(|e| FfiError::InvalidInput {
            msg: format!("multiaddr invalide: {e}"),
        })?;
        Ok(self.inner.listen(addr).await?.to_string())
    }

    /// Se connecte à un pair `/ip4/.../tcp/.../p2p/<peerid>`.
    pub async fn connect(&self, peer: String) -> Result<(), FfiError> {
        let addr = peer.parse().map_err(|e| FfiError::InvalidInput {
            msg: format!("multiaddr invalide: {e}"),
        })?;
        self.inner.connect(addr).await?;
        Ok(())
    }

    /// Ingestion d'un média (ffmpeg → HLS). Renvoie le CID du manifeste.
    pub async fn ingest_file(&self, path: String) -> Result<String, FfiError> {
        let cid = self.inner.ingest_file(std::path::Path::new(&path)).await?;
        Ok(cid.to_string())
    }

    /// Publie un feed signé avec métadonnées (titre, tags, provenance) : rend
    /// le contenu **cherchable** (index local des pairs + découverte par tag
    /// via la DHT).
    pub async fn publish_feed_with(&self, items: Vec<FfiContentItem>) -> Result<(), FfiError> {
        // Valide les CIDs avant signature (erreur typée InvalidInput).
        parse_cids(&items.iter().map(|i| i.cid.clone()).collect::<Vec<_>>())?;
        let entries: Vec<crate::feed::FeedEntry> = items
            .into_iter()
            .map(|i| crate::feed::FeedEntry {
                cid: i.cid,
                title: i.title,
                tags: i.tags,
                provenance: i.provenance.into(),
            })
            .collect();
        self.inner.publish_feed_with(&entries).await?;
        Ok(())
    }

    /// Recherche par tag **via la DHT** (découverte hors gossip) : retrouve les
    /// émetteurs fournisseurs du tag et vérifie leurs feeds signés.
    pub async fn search_tag(&self, tag: String) -> Result<Vec<FfiSearchHit>, FfiError> {
        Ok(self
            .inner
            .search_tag(&tag)
            .await?
            .into_iter()
            .map(search_hit_to_ffi)
            .collect())
    }

    /// Enregistre un listener de catalogue : remplace l'attente aveugle côté
    /// front (délai fixe après `connect`) par un rafraîchissement réactif.
    /// Chaque appel ajoute un abonnement qui vit aussi longtemps que le nœud.
    pub async fn set_catalog_listener(&self, listener: Arc<dyn CatalogListener>) {
        let mut events = self.inner.subscribe_catalog();
        // La tâche s'arrête quand le canal ferme (nœud abandonné). Un abonné en
        // retard (Lagged) a seulement raté des tics intermédiaires : on notifie
        // quand même, l'instantané `catalog()` relu par le front est à jour.
        tokio::spawn(async move {
            use tokio::sync::broadcast::error::RecvError;
            while let Ok(()) | Err(RecvError::Lagged(_)) = events.recv().await {
                listener.on_catalog_updated();
            }
        });
    }

    /// Souscrit à un éditeur de denylist (lien `champinium://denylist/<peerid>`
    /// ou PeerId nu) : ajoute l'éditeur à `.denylist_issuers`, persisté, puis
    /// déclenche un fetch immédiat en tâche de fond (contrat v13, ADR 0011).
    pub async fn subscribe_denylist_issuer(&self, link_or_peer_id: String) -> Result<(), FfiError> {
        let peer = crate::channel_link::parse_denylist(&link_or_peer_id).map_err(|_| {
            FfiError::InvalidInput {
                msg: format!("lien/PeerId de denylist invalide: {link_or_peer_id}"),
            }
        })?;
        self.inner.subscribe_denylist_issuer(peer)?;
        Ok(())
    }

    /// Se désabonne d'un éditeur de denylist. Refuse l'éditeur projet (denylist
    /// par défaut, non désactivable) avec `InvalidInput`.
    pub async fn unsubscribe_denylist_issuer(&self, peer_id: String) -> Result<(), FfiError> {
        let peer = parse_peer_id(&peer_id)?;
        self.inner.unsubscribe_denylist_issuer(peer).await?;
        Ok(())
    }

    /// Enregistre un listener de modération : même patron que
    /// `set_catalog_listener` — remplace le polling côté front par un
    /// rafraîchissement réactif de `denylist_sources()`.
    pub async fn set_moderation_listener(&self, listener: Arc<dyn ModerationListener>) {
        let mut events = self.inner.subscribe_moderation();
        tokio::spawn(async move {
            use tokio::sync::broadcast::error::RecvError;
            loop {
                match events.recv().await {
                    Ok(()) => listener.on_moderation_updated(),
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                }
            }
        });
    }

    /// Définit le profil de channel : persisté, republie le feed courant.
    pub async fn set_channel_profile(&self, profile: FfiChannelProfile) -> Result<(), FfiError> {
        if profile.name.len() > crate::feed::MAX_CHANNEL_NAME_LEN
            || profile.description.len() > crate::feed::MAX_CHANNEL_DESC_LEN
        {
            return Err(FfiError::InvalidInput {
                msg: "profil de channel hors bornes".into(),
            });
        }
        if let Some(avatar) = &profile.avatar_cid {
            avatar.parse::<Cid>().map_err(|e| FfiError::InvalidInput {
                msg: format!("avatar_cid invalide: {e}"),
            })?;
        }
        self.inner
            .set_channel_profile(crate::feed::ChannelMeta {
                name: profile.name,
                description: profile.description,
                avatar_cid: profile.avatar_cid,
            })
            .await?;
        Ok(())
    }

    /// S'abonne à un émetteur (lien `champinium://channel/<peerid>` ou PeerId
    /// nu) : persiste immédiatement (état local privé, jamais publié), puis
    /// déclenche un fetch immédiat en tâche de fond.
    pub async fn subscribe_channel(&self, link_or_peer_id: String) -> Result<(), FfiError> {
        let peer = crate::channel_link::parse(&link_or_peer_id)
            .map_err(|e| FfiError::InvalidInput { msg: e.to_string() })?;
        self.inner.subscribe(peer)?;
        Ok(())
    }

    /// Se désabonne d'un émetteur.
    pub async fn unsubscribe_channel(&self, peer_id: String) -> Result<(), FfiError> {
        let peer = parse_peer_id(&peer_id)?;
        self.inner.unsubscribe(peer)?;
        Ok(())
    }

    /// Résout un aperçu de channel par lien `champinium://channel/<peerid>`
    /// (ou PeerId nu, même tolérance que `subscribe_channel`) : catalogue
    /// d'abord, sinon DHT (voir `Node::resolve_channel`). Un émetteur bloqué
    /// localement reste résolvable (`blocked = true`) ; un émetteur introuvable
    /// (aucun fournisseur) renvoie `NotFound`.
    pub async fn resolve_channel(
        &self,
        link_or_peer_id: String,
    ) -> Result<FfiChannelPreview, FfiError> {
        // Parse tolérant explicite : PAS de `?` direct sur `channel_link::parse`,
        // son erreur n'est pas un `CoreError` et ne doit surtout pas retomber
        // dans le mapping `Identity` → `Internal` (piège documenté sur
        // `subscribe_channel`/`block_channel`).
        let peer = crate::channel_link::parse(&link_or_peer_id)
            .map_err(|e| FfiError::InvalidInput { msg: e.to_string() })?;
        let preview = self.inner.resolve_channel(peer).await?;
        Ok(channel_preview_to_ffi(preview))
    }

    /// Bloque un channel **localement** (lien `champinium://channel/<peerid>`
    /// ou PeerId nu, même tolérance que `subscribe_channel`) : préférence
    /// strictement privée de ce nœud, jamais publiée. Désabonne si souscrit,
    /// purge catalogue/SeedIndex/blockstore de ce qui lui était attribué.
    pub async fn block_channel(&self, link_or_peer_id: String) -> Result<(), FfiError> {
        let peer = crate::channel_link::parse(&link_or_peer_id)
            .map_err(|e| FfiError::InvalidInput { msg: e.to_string() })?;
        self.inner.block_channel(peer).await?;
        Ok(())
    }

    /// Débloque un channel bloqué localement.
    pub async fn unblock_channel(&self, peer_id: String) -> Result<(), FfiError> {
        let peer = parse_peer_id(&peer_id)?;
        self.inner.unblock_channel(peer)?;
        Ok(())
    }

    /// Définit le quota de seed proactif (octets) : persiste puis réveille la
    /// boucle de seed (une baisse peut nécessiter une éviction immédiate).
    pub async fn set_seed_quota(&self, bytes: u64) -> Result<(), FfiError> {
        self.inner.set_seed_quota(bytes)?;
        Ok(())
    }

    /// Épingle un manifeste : exempté d'éviction par le seed proactif, qu'il
    /// soit ou non déjà retenu.
    pub async fn pin_content(&self, manifest_cid: String) -> Result<(), FfiError> {
        let cid: Cid = manifest_cid.parse().map_err(|e| FfiError::InvalidInput {
            msg: format!("CID invalide: {e}"),
        })?;
        self.inner.pin(cid)?;
        Ok(())
    }

    /// Retire l'épinglage d'un manifeste (redevient évictable sous quota).
    pub async fn unpin_content(&self, manifest_cid: String) -> Result<(), FfiError> {
        let cid: Cid = manifest_cid.parse().map_err(|e| FfiError::InvalidInput {
            msg: format!("CID invalide: {e}"),
        })?;
        self.inner.unpin(cid)?;
        Ok(())
    }

    /// Enregistre un listener de seed proactif : même patron que
    /// `set_catalog_listener` — remplace le polling côté front par un
    /// rafraîchissement réactif de `storage_stats()`/`catalog()`.
    pub async fn set_seed_listener(&self, listener: Arc<dyn SeedListener>) {
        let mut events = self.inner.subscribe_seed();
        tokio::spawn(async move {
            use tokio::sync::broadcast::error::RecvError;
            while let Ok(()) | Err(RecvError::Lagged(_)) = events.recv().await {
                listener.on_seed_updated();
            }
        });
    }

    /// Ouvre une session de lecture progressive et rend l'URL HLS locale.
    /// `Moderated`/`NotFound` sortent ici (manifeste), avant tout serveur.
    pub async fn open_stream(&self, manifest_cid: String) -> Result<FfiStreamSession, FfiError> {
        let cid: Cid = manifest_cid.parse().map_err(|e| FfiError::InvalidInput {
            msg: format!("CID invalide: {e}"),
        })?;
        let s = self.inner.open_stream(cid).await?;
        Ok(FfiStreamSession {
            id: s.id,
            url: s.url,
            total_segments: s.total_segments,
        })
    }

    /// Ferme une session (idempotent) : serveur arrêté, cache purgé.
    pub async fn close_stream(&self, id: u64) {
        self.inner.close_stream(id).await;
    }

    /// Même patron que `set_seed_listener`.
    pub async fn set_stream_listener(&self, listener: Arc<dyn StreamListener>) {
        let mut events = self.inner.subscribe_stream();
        tokio::spawn(async move {
            use tokio::sync::broadcast::error::RecvError;
            loop {
                match events.recv().await {
                    Ok(id) => listener.on_stream_updated(id),
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                }
            }
        });
    }
}

fn catalog_entry_to_ffi(node: &Node, e: crate::catalog::CatalogEntry) -> FfiCatalogEntry {
    // `seed_coverage(&e.cids)` — un seul verrou `seed_index` sur les CIDs déjà
    // en main, PAS `seed_status`/`pinned_manifests_of` (chacun reconstruirait
    // tout le catalogue pour retrouver CETTE entrée : O(N²) sur l'ensemble du
    // catalogue à chaque `catalog()`/`catalog_subscribed()`, exactement le
    // chemin chaud du rafraîchissement réactif Seed/CatalogListener — retour
    // de review, tâche c5).
    let (seeded_count, total_count, pinned) = node.seed_coverage(&e.cids);
    let pinned = pinned.into_iter().map(|c| c.to_string()).collect();
    FfiCatalogEntry {
        issuer: e.issuer.to_string(),
        seq: e.seq,
        cids: e.cids.iter().map(|c| c.to_string()).collect(),
        items: e
            .items
            .into_iter()
            .map(|i| FfiContentItem {
                cid: i.cid.to_string(),
                title: i.title,
                tags: i.tags,
                provenance: i.provenance.into(),
            })
            .collect(),
        channel: FfiChannelProfile {
            name: e.channel.name,
            description: e.channel.description,
            avatar_cid: e.channel.avatar_cid,
        },
        // `entry.cids` est borné par les mêmes plafonds anti-DoS que le
        // catalogue (taille de feed + nombre d'émetteurs, cf. durcissement
        // post-audit Phase 4) : bien en-deçà de `u32::MAX`, la troncature est
        // structurellement impossible aujourd'hui.
        seeded_count: seeded_count as u32,
        total_count: total_count as u32,
        pinned,
    }
}

fn channel_preview_to_ffi(p: crate::p2p::ChannelPreview) -> FfiChannelPreview {
    FfiChannelPreview {
        peer_id: p.issuer.to_string(),
        name: p.channel.name,
        description: p.channel.description,
        avatar_cid: p.channel.avatar_cid,
        items: p
            .items
            .into_iter()
            .map(|i| FfiContentItem {
                cid: i.cid.to_string(),
                title: i.title,
                tags: i.tags,
                provenance: i.provenance.into(),
            })
            .collect(),
        subscribed: p.subscribed,
        blocked: p.blocked,
    }
}

/// Parse un PeerId nu fourni par un front ; erreur typée InvalidInput.
fn parse_peer_id(s: &str) -> Result<crate::PeerId, FfiError> {
    s.parse::<crate::PeerId>()
        .map_err(|e| FfiError::InvalidInput {
            msg: format!("PeerId invalide '{s}': {e}"),
        })
}

fn search_hit_to_ffi(h: crate::catalog::SearchHit) -> FfiSearchHit {
    FfiSearchHit {
        issuer: h.issuer.to_string(),
        cid: h.cid.to_string(),
        title: h.title,
        tags: h.tags,
        provenance: h.provenance.into(),
    }
}

fn parse_cids(cids: &[String]) -> Result<Vec<Cid>, FfiError> {
    cids.iter()
        .map(|c| {
            c.parse::<Cid>().map_err(|e| FfiError::InvalidInput {
                msg: format!("CID invalide '{c}': {e}"),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CoreError;

    /// Les fronts doivent pouvoir distinguer par programme un refus de
    /// modération (UX « contenu bloqué ») d'une erreur réseau ou d'une entrée
    /// invalide — le contrat expose donc des variantes typées, pas une chaîne.
    #[test]
    fn core_errors_map_to_typed_ffi_variants() {
        assert!(matches!(
            FfiError::from(CoreError::Moderated("cid".into())),
            FfiError::Moderated { .. }
        ));
        assert!(matches!(
            FfiError::from(CoreError::Network("boom".into())),
            FfiError::Network { .. }
        ));
        assert!(matches!(
            FfiError::from(CoreError::NoProviders("cid".into())),
            FfiError::NotFound { .. }
        ));
        assert!(matches!(
            FfiError::from(CoreError::BlockNotFound("cid".into())),
            FfiError::NotFound { .. }
        ));
        assert!(matches!(
            FfiError::from(CoreError::Ingest("ffmpeg".into())),
            FfiError::Internal { .. }
        ));
    }

    /// Le message d'origine reste porté par la variante (diagnostic).
    #[test]
    fn typed_variants_carry_the_message() {
        let e = FfiError::from(CoreError::Moderated("bafy…".into()));
        assert!(e.to_string().contains("bafy…"));
    }

    /// Une entrée invalide côté front (multiaddr, CID) est une InvalidInput.
    #[test]
    fn invalid_cid_input_maps_to_invalid_input() {
        assert!(matches!(
            parse_cids(&["pas-un-cid".into()]).unwrap_err(),
            FfiError::InvalidInput { .. }
        ));
    }

    /// Contrat v12 : la provenance déclarée traverse `publish_feed_with` →
    /// catalogue → recherche (outils normalisés, dédupliqués comme les tags).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn provenance_roundtrips_through_publish_and_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let node = open_node(dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();
        let cid = node.inner.add(b"contenu ia").await.unwrap();
        node.publish_feed_with(vec![FfiContentItem {
            cid: cid.to_string(),
            title: "Dunes".into(),
            tags: vec!["desert".into()],
            provenance: FfiProvenance {
                mode: FfiProvenanceMode::Generated,
                tools: vec![" Sora ".into(), "sora".into()],
            },
        }])
        .await
        .unwrap();
        let entry = node
            .catalog()
            .into_iter()
            .find(|e| e.issuer == node.peer_id())
            .expect("le créateur figure dans son catalogue");
        let item = &entry.items[0];
        assert!(matches!(item.provenance.mode, FfiProvenanceMode::Generated));
        assert_eq!(item.provenance.tools, vec!["sora".to_string()]);
        let hits = node.search("sora".into());
        assert_eq!(hits.len(), 1);
        assert!(matches!(
            hits[0].provenance.mode,
            FfiProvenanceMode::Generated
        ));
        assert_eq!(crate::contract_version(), 13);
    }

    /// Le listener enregistré est rappelé quand le catalogue change — c'est le
    /// mécanisme qui remplace le délai gossip codé en dur dans les fronts.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn catalog_listener_fires_when_catalog_changes() {
        struct Probe(std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>);
        impl CatalogListener for Probe {
            fn on_catalog_updated(&self) {
                if let Some(tx) = self.0.lock().unwrap().take() {
                    let _ = tx.send(());
                }
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let node = open_node(dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();

        let (tx, rx) = tokio::sync::oneshot::channel();
        node.set_catalog_listener(Arc::new(Probe(std::sync::Mutex::new(Some(tx)))))
            .await;

        let cid = crate::content::cid_for(b"contenu ffi").to_string();
        node.publish_feed_with(vec![FfiContentItem {
            cid,
            title: String::new(),
            tags: vec![],
            provenance: FfiProvenance {
                mode: FfiProvenanceMode::Undeclared,
                tools: vec![],
            },
        }])
        .await
        .unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(5), rx)
            .await
            .expect("le listener doit être rappelé après publish_feed_with")
            .unwrap();
    }

    /// Contrat v4 : publication avec métadonnées + recherche locale exposées
    /// aux fronts (le catalogue expose aussi les items enrichis).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn publish_with_metadata_then_search_via_contract() {
        let dir = tempfile::tempdir().unwrap();
        let node = open_node(dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();

        let cid = crate::content::cid_for(b"contenu v4").to_string();
        node.publish_feed_with(vec![FfiContentItem {
            cid: cid.clone(),
            title: "Aurores boréales".into(),
            tags: vec!["nature".into()],
            provenance: FfiProvenance {
                mode: FfiProvenanceMode::Undeclared,
                tools: vec![],
            },
        }])
        .await
        .unwrap();

        // Recherche locale (titre, insensible à la casse).
        let hits = node.search("aurores".into());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].cid, cid);
        assert_eq!(hits[0].title, "Aurores boréales");

        // Le catalogue expose les items enrichis.
        let entries = node.catalog();
        assert_eq!(entries[0].items[0].title, "Aurores boréales");
        assert_eq!(entries[0].items[0].tags, vec!["nature"]);
    }

    /// Contrat v5 : profil de channel — défini via FFI, porté par le catalogue.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn channel_profile_flows_through_contract() {
        let dir = tempfile::tempdir().unwrap();
        let node = open_node(dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();

        node.set_channel_profile(FfiChannelProfile {
            name: "Aurores".into(),
            description: "Ciels nocturnes".into(),
            avatar_cid: None,
        })
        .await
        .unwrap();
        assert_eq!(node.channel_profile().name, "Aurores");

        let cid = crate::content::cid_for(b"contenu v5").to_string();
        node.publish_feed_with(vec![FfiContentItem {
            cid,
            title: String::new(),
            tags: vec![],
            provenance: FfiProvenance {
                mode: FfiProvenanceMode::Undeclared,
                tools: vec![],
            },
        }])
        .await
        .unwrap();
        assert_eq!(node.catalog()[0].channel.name, "Aurores");
    }

    /// Un profil hors bornes est une InvalidInput (UX front : erreur de saisie).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn oversized_profile_is_invalid_input() {
        let dir = tempfile::tempdir().unwrap();
        let node = open_node(dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();
        let err = node
            .set_channel_profile(FfiChannelProfile {
                name: "n".repeat(crate::feed::MAX_CHANNEL_NAME_LEN + 1),
                description: String::new(),
                avatar_cid: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, FfiError::InvalidInput { .. }), "{err}");
    }

    /// Contrat v6 : `subscribe_channel` accepte un lien `champinium://channel/…`
    /// et fait apparaître le PeerId dans `subscriptions()`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subscribe_channel_with_link_appears_in_subscriptions() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let a = open_node(dir_a.path().to_string_lossy().into_owned())
            .await
            .unwrap();
        let b = open_node(dir_b.path().to_string_lossy().into_owned())
            .await
            .unwrap();

        let link = a.channel_link(a.peer_id()).unwrap();
        b.subscribe_channel(link).await.unwrap();

        assert_eq!(b.subscriptions(), vec![a.peer_id()]);
    }

    /// Une entrée invalide (ni lien, ni PeerId nu) est une InvalidInput.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subscribe_channel_with_garbage_is_invalid_input() {
        let dir = tempfile::tempdir().unwrap();
        let node = open_node(dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();

        let err = node
            .subscribe_channel("nimporte quoi".into())
            .await
            .unwrap_err();
        assert!(matches!(err, FfiError::InvalidInput { .. }), "{err}");
    }

    /// Après `subscribe_channel` d'un émetteur présent au catalogue, celui-ci
    /// apparaît dans `catalog_subscribed()`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn catalog_subscribed_reflects_subscription() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let a = open_node(dir_a.path().to_string_lossy().into_owned())
            .await
            .unwrap();
        let b = open_node(dir_b.path().to_string_lossy().into_owned())
            .await
            .unwrap();

        let cid = crate::content::cid_for(b"contenu v6").to_string();
        a.publish_feed_with(vec![FfiContentItem {
            cid,
            title: String::new(),
            tags: vec![],
            provenance: FfiProvenance {
                mode: FfiProvenanceMode::Undeclared,
                tools: vec![],
            },
        }])
        .await
        .unwrap();
        // Injecte directement l'entrée de a dans le catalogue de b (pas de
        // réseau dans ce test) : on souscrit puis on peuple le catalogue via
        // le feed déjà signé de a, appliqué localement.
        for entry in a.catalog() {
            assert_eq!(entry.issuer, a.peer_id());
        }

        b.subscribe_channel(a.peer_id()).await.unwrap();
        assert_eq!(b.subscriptions(), vec![a.peer_id()]);
        // Sans réseau connecté, catalog_subscribed peut rester vide côté b ;
        // on vérifie surtout qu'il ne contient QUE des émetteurs souscrits.
        for entry in b.catalog_subscribed() {
            assert_eq!(entry.issuer, a.peer_id());
        }
    }

    /// `channel_link` produit un lien que `subscribe_channel` accepte, et
    /// rejette un PeerId invalide.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn channel_link_roundtrips_through_ffi() {
        let dir = tempfile::tempdir().unwrap();
        let node = open_node(dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();

        let link = node.channel_link(node.peer_id()).unwrap();
        assert!(link.starts_with("champinium://channel/"));

        let err = node.channel_link("pas-un-peerid".into()).unwrap_err();
        assert!(matches!(err, FfiError::InvalidInput { .. }), "{err}");
    }

    /// Contrat v7 : le quota de seed défini via `set_seed_quota` est persisté —
    /// un nœud rouvert sur le même répertoire retrouve la même valeur (comme le
    /// `seq` de feed ou les abonnements, chargés au constructeur).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn seed_quota_roundtrip_persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().to_string_lossy().into_owned();

        let node = open_node(data_dir.clone()).await.unwrap();
        node.set_seed_quota(12_345).await.unwrap();
        assert_eq!(node.seed_quota(), 12_345);
        assert_eq!(node.storage_stats().quota_bytes, 12_345);
        drop(node);

        let reopened = open_node(data_dir).await.unwrap();
        assert_eq!(reopened.seed_quota(), 12_345);
    }

    /// Un nœud fraîchement ouvert n'a rien seedé : `storage_stats` reflète un
    /// index de seed vide sous le quota par défaut.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn storage_stats_is_zero_on_fresh_node() {
        let dir = tempfile::tempdir().unwrap();
        let node = open_node(dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();
        let stats = node.storage_stats();
        assert_eq!(stats.used_bytes, 0);
        assert!(stats.quota_bytes > 0);
    }

    /// Une entrée de catalogue enrichit `seeded_count`/`total_count`/`pinned` à
    /// partir du `SeedIndex` local : ici la publication n'est pas retenue au
    /// seed (aucun octet réellement stocké — `seeded_count` reste à 0) mais
    /// `pin_content` marque quand même le manifeste comme épinglé, et
    /// `unpin_content` fait le chemin inverse — le contrat n'exige pas qu'une
    /// publication soit seedée pour être épinglable.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn catalog_entry_exposes_seed_fields_and_pin_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let node = open_node(dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();

        let cid = crate::content::cid_for(b"contenu v7").to_string();
        node.publish_feed_with(vec![FfiContentItem {
            cid: cid.clone(),
            title: String::new(),
            tags: vec![],
            provenance: FfiProvenance {
                mode: FfiProvenanceMode::Undeclared,
                tools: vec![],
            },
        }])
        .await
        .unwrap();

        let entry = &node.catalog()[0];
        assert_eq!(entry.total_count, 1);
        assert_eq!(entry.seeded_count, 0);
        assert!(entry.pinned.is_empty());

        node.pin_content(cid.clone()).await.unwrap();
        let entry = &node.catalog()[0];
        assert_eq!(entry.pinned, vec![cid.clone()]);

        node.unpin_content(cid).await.unwrap();
        assert!(node.catalog()[0].pinned.is_empty());
    }

    /// Un CID invalide fourni à `pin_content`/`unpin_content` est une
    /// `InvalidInput` (même contrat que les autres entrées CID de la surface).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pin_and_unpin_content_reject_invalid_cid() {
        let dir = tempfile::tempdir().unwrap();
        let node = open_node(dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();

        assert!(matches!(
            node.pin_content("pas-un-cid".into()).await.unwrap_err(),
            FfiError::InvalidInput { .. }
        ));
        assert!(matches!(
            node.unpin_content("pas-un-cid".into()).await.unwrap_err(),
            FfiError::InvalidInput { .. }
        ));
    }

    /// `set_seed_listener` : même patron que `set_catalog_listener`. Le seed
    /// proactif émet un tic à l'ingestion d'un contenu propre (retenu +
    /// épinglé d'office) — `storage_stats` est alors cohérent avec les octets
    /// réellement stockés (publication + seed dans le même mouvement).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn seed_listener_fires_after_ingest_and_storage_stats_is_coherent() {
        if !ffmpeg_available().await {
            eprintln!("ffmpeg absent — test seed_listener/ingest ignoré");
            return;
        }

        struct Probe(std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>);
        impl SeedListener for Probe {
            fn on_seed_updated(&self) {
                if let Some(tx) = self.0.lock().unwrap().take() {
                    let _ = tx.send(());
                }
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let node = open_node(dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();

        let (tx, rx) = tokio::sync::oneshot::channel();
        node.set_seed_listener(Arc::new(Probe(std::sync::Mutex::new(Some(tx)))))
            .await;

        let input = dir.path().join("input.mp4");
        assert!(generate_media(&input).await, "génération du média de test");
        let manifest_cid = node
            .ingest_file(input.to_string_lossy().into_owned())
            .await
            .unwrap();
        // `ingest_file` seede/épingle le contenu propre mais ne publie pas de
        // feed : sans `publish_feed_with`, aucune entrée de catalogue ne
        // référence le manifeste (le catalogue reconstruit uniquement depuis
        // les feeds).
        node.publish_feed_with(vec![FfiContentItem {
            cid: manifest_cid.clone(),
            title: String::new(),
            tags: vec![],
            provenance: FfiProvenance {
                mode: FfiProvenanceMode::Undeclared,
                tools: vec![],
            },
        }])
        .await
        .unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(5), rx)
            .await
            .expect("le SeedListener doit être rappelé après ingest_file")
            .unwrap();

        let stats = node.storage_stats();
        assert!(
            stats.used_bytes > 0,
            "le contenu propre ingéré doit être compté dans storage_stats"
        );
        assert!(stats.used_bytes <= stats.quota_bytes);

        // Le contenu propre est épinglé d'office (jamais évincé) : le contrat
        // v7 doit le refléter dans `pinned` de l'entrée de catalogue.
        let entries = node.catalog();
        let own = entries.iter().find(|e| e.issuer == node.peer_id()).expect(
            "le catalogue doit contenir l'entrée du nœud lui-même après ingest_file + publish",
        );
        assert!(own.pinned.contains(&manifest_cid));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn open_stream_via_ffi_returns_url_and_listener_fires() {
        struct Probe(std::sync::Mutex<Option<tokio::sync::oneshot::Sender<u64>>>);
        impl StreamListener for Probe {
            fn on_stream_updated(&self, id: u64) {
                if let Some(tx) = self.0.lock().unwrap().take() {
                    let _ = tx.send(id);
                }
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let node = open_node(dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        node.set_stream_listener(Arc::new(Probe(std::sync::Mutex::new(Some(tx)))))
            .await;

        let seg = node.inner.add(b"seg").await.unwrap();
        let manifest = crate::ingest::HlsManifest::new(
            1.0,
            vec![crate::ingest::HlsSegment {
                cid: seg.to_string(),
                duration: 1.0,
            }],
        );
        let m = node
            .inner
            .add(manifest.to_json().unwrap().as_bytes())
            .await
            .unwrap();
        let s = node.open_stream(m.to_string()).await.unwrap();
        assert!(s.url.starts_with("http://127.0.0.1:"));
        assert_eq!(s.total_segments, 1);
        let fired = tokio::time::timeout(std::time::Duration::from_secs(10), rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fired, s.id);
        assert_eq!(node.stream_status(s.id).unwrap().total_segments, 1);
        node.close_stream(s.id).await;
        assert!(matches!(
            node.stream_status(s.id),
            Err(FfiError::NotFound { .. })
        ));
        assert!(matches!(
            node.open_stream("pas-un-cid".into()).await,
            Err(FfiError::InvalidInput { .. })
        ));
    }

    /// Contrat v8 : `block_channel` accepte un lien `champinium://channel/…`
    /// et fait apparaître le PeerId dans `blocked_channels()`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn block_channel_with_link_appears_in_blocked_channels() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let a = open_node(dir_a.path().to_string_lossy().into_owned())
            .await
            .unwrap();
        let b = open_node(dir_b.path().to_string_lossy().into_owned())
            .await
            .unwrap();

        let link = a.channel_link(a.peer_id()).unwrap();
        b.block_channel(link).await.unwrap();

        assert_eq!(b.blocked_channels(), vec![a.peer_id()]);
    }

    /// `subscribe_channel` visant un émetteur bloqué localement doit remonter
    /// `FfiError::Moderated` (UX « refus volontaire »), PAS `InvalidInput`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subscribe_channel_of_blocked_peer_is_moderated() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let a = open_node(dir_a.path().to_string_lossy().into_owned())
            .await
            .unwrap();
        let b = open_node(dir_b.path().to_string_lossy().into_owned())
            .await
            .unwrap();

        b.block_channel(a.peer_id()).await.unwrap();

        let err = b.subscribe_channel(a.peer_id()).await.unwrap_err();
        assert!(matches!(err, FfiError::Moderated { .. }), "{err}");
    }

    /// Une entrée invalide (ni lien, ni PeerId nu) est une InvalidInput.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn block_channel_with_garbage_is_invalid_input() {
        let dir = tempfile::tempdir().unwrap();
        let node = open_node(dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();

        let err = node
            .block_channel("nimporte quoi".into())
            .await
            .unwrap_err();
        assert!(matches!(err, FfiError::InvalidInput { .. }), "{err}");
    }

    /// `unblock_channel` fait le chemin inverse de `block_channel`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unblock_channel_roundtrips() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let a = open_node(dir_a.path().to_string_lossy().into_owned())
            .await
            .unwrap();
        let b = open_node(dir_b.path().to_string_lossy().into_owned())
            .await
            .unwrap();

        b.block_channel(a.peer_id()).await.unwrap();
        assert_eq!(b.blocked_channels(), vec![a.peer_id()]);

        b.unblock_channel(a.peer_id()).await.unwrap();
        assert!(b.blocked_channels().is_empty());

        // Débloqué : `subscribe_channel` doit maintenant réussir.
        b.subscribe_channel(a.peer_id()).await.unwrap();
    }

    /// Contrat v9 : `resolve_channel` sur son propre channel (publication
    /// locale, sans réseau) renvoie un aperçu peuplé — profil, items,
    /// `subscribed = false` (on ne s'abonne jamais à soi-même).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn resolve_channel_local_publication_populates_preview() {
        let dir = tempfile::tempdir().unwrap();
        let node = open_node(dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();

        node.set_channel_profile(FfiChannelProfile {
            name: "Aurores".into(),
            description: "Ciels nocturnes".into(),
            avatar_cid: None,
        })
        .await
        .unwrap();

        let cid = crate::content::cid_for(b"contenu v9").to_string();
        node.publish_feed_with(vec![FfiContentItem {
            cid: cid.clone(),
            title: "Aurores boréales".into(),
            tags: vec!["nature".into()],
            provenance: FfiProvenance {
                mode: FfiProvenanceMode::Undeclared,
                tools: vec![],
            },
        }])
        .await
        .unwrap();

        let link = node.channel_link(node.peer_id()).unwrap();
        let preview = node.resolve_channel(link).await.unwrap();

        assert_eq!(preview.peer_id, node.peer_id());
        assert_eq!(preview.name, "Aurores");
        assert_eq!(preview.items.len(), 1);
        assert_eq!(preview.items[0].cid, cid);
        assert!(!preview.subscribed);
        assert!(!preview.blocked);
    }

    /// Garde anti-inversion : `subscribed`/`blocked` doivent traverser le FFI
    /// dans le bon ordre. Un nœud n'a pas de garde-fou contre l'auto-abonnement
    /// (`Node::subscribe` n'exclut pas `self`) — le chemin le plus simple pour
    /// obtenir `subscribed = true` ET `blocked = false` simultanément sur le
    /// MÊME aperçu, sans dépendre du réseau. Un swap
    /// (`subscribed: p.blocked, blocked: p.subscribed` dans
    /// `channel_preview_to_ffi`) ferait échouer soit cette assertion soit celle
    /// du test précédent (qui attend `subscribed = false`).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn resolve_channel_preview_reflects_subscribed_not_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let node = open_node(dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();

        let cid = crate::content::cid_for(b"contenu v9 auto-abonnement").to_string();
        node.publish_feed_with(vec![FfiContentItem {
            cid,
            title: String::new(),
            tags: vec![],
            provenance: FfiProvenance {
                mode: FfiProvenanceMode::Undeclared,
                tools: vec![],
            },
        }])
        .await
        .unwrap();
        node.subscribe_channel(node.peer_id()).await.unwrap();

        let preview = node.resolve_channel(node.peer_id()).await.unwrap();
        assert!(preview.subscribed);
        assert!(!preview.blocked);
    }

    /// Une entrée invalide (ni lien, ni PeerId nu) est une InvalidInput.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn resolve_channel_with_garbage_is_invalid_input() {
        let dir = tempfile::tempdir().unwrap();
        let node = open_node(dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();

        let result = node.resolve_channel("nimporte quoi".into()).await;
        assert!(matches!(result, Err(FfiError::InvalidInput { .. })));
    }

    /// `resolve_channel` accepte un PeerId nu (même tolérance que
    /// `subscribe_channel`/`block_channel`), pas seulement un lien.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn resolve_channel_accepts_bare_peer_id() {
        let dir = tempfile::tempdir().unwrap();
        let node = open_node(dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();

        let cid = crate::content::cid_for(b"contenu v9 bare").to_string();
        node.publish_feed_with(vec![FfiContentItem {
            cid,
            title: String::new(),
            tags: vec![],
            provenance: FfiProvenance {
                mode: FfiProvenanceMode::Undeclared,
                tools: vec![],
            },
        }])
        .await
        .unwrap();

        let preview = node.resolve_channel(node.peer_id()).await.unwrap();
        assert_eq!(preview.peer_id, node.peer_id());
    }

    /// Un émetteur inconnu/injoignable (aucun fournisseur DHT) est un
    /// `NotFound`, pas une `Internal`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn resolve_channel_of_unreachable_issuer_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let node = open_node(dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();

        let unknown = crate::PeerId::random();
        let result = node.resolve_channel(unknown.to_string()).await;
        assert!(matches!(result, Err(FfiError::NotFound { .. })));
    }

    /// Décision A (tâche CS-b 1) : la surface de bascule du repli froid
    /// existe et fonctionne dans le build FFI par défaut, SANS la feature
    /// `cold-storage` — même contrat quel que soit le build.
    #[tokio::test]
    async fn ffi_cold_retrieval_toggle_roundtrips_default_build() {
        let dir = tempfile::tempdir().unwrap();
        let node = open_node(dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();
        assert!(node.cold_retrieval_enabled());
        node.set_cold_retrieval(false).unwrap();
        assert!(!node.cold_retrieval_enabled());
    }

    /// Contrat v13 : l'éditeur projet est toujours présent, verrouillé, et
    /// non abonné avant premier fetch (`fetched = false`) ; un nouvel éditeur
    /// s'ajoute via `subscribe_denylist_issuer` (lien) et n'est pas verrouillé.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn denylist_sources_list_locked_project_issuer() {
        let dir = tempfile::tempdir().unwrap();
        let node = open_node(dir.path().to_string_lossy().into_owned())
            .await
            .unwrap();
        let sources = node.denylist_sources();
        assert_eq!(sources.len(), 1);
        assert!(sources[0].locked && !sources[0].fetched);
        assert!(matches!(
            node.unsubscribe_denylist_issuer(sources[0].peer_id.clone())
                .await,
            Err(FfiError::InvalidInput { .. })
        ));
        let other = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        node.subscribe_denylist_issuer(node.denylist_link(other.to_string()).unwrap())
            .await
            .unwrap();
        assert_eq!(node.denylist_sources().len(), 2);
        assert!(matches!(
            node.subscribe_denylist_issuer("pas-un-peerid".into()).await,
            Err(FfiError::InvalidInput { .. })
        ));
        assert_eq!(crate::contract_version(), 13);
    }

    async fn ffmpeg_available() -> bool {
        tokio::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    async fn generate_media(out: &std::path::Path) -> bool {
        tokio::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "lavfi", "-i"])
            .arg("testsrc=duration=1:size=160x90:rate=15")
            .args(["-f", "lavfi", "-i"])
            .arg("sine=frequency=440:duration=1")
            .args(["-c:v", "libx264", "-c:a", "aac", "-shortest"])
            .arg(out)
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }
}
