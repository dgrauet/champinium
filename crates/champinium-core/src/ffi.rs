//! Surface UniFFI exposée aux fronts natifs (contrat v1).
//!
//! C'est la **frontière de contrat** : les fronts (Swift, C#) codent contre cet
//! objet, jamais contre l'implémentation. Toute évolution passe par l'agent NOYAU
//! et incrémente `CONTRACT_VERSION` (voir `/AGENTS.md`).
//!
//! Le risque #1 (async via FFI) est éprouvé ici : la plupart des méthodes sont
//! `async` (runtime tokio) et exposées vers Swift ET C#.

use crate::{Denylist, Node};
use cid::Cid;
use std::path::PathBuf;
use std::sync::Arc;

/// Erreur exposée aux fronts (message aplati).
#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum FfiError {
    #[error("{0}")]
    Failed(String),
}

impl From<crate::CoreError> for FfiError {
    fn from(e: crate::CoreError) -> Self {
        FfiError::Failed(e.to_string())
    }
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
}

/// Nœud Champinium exposé aux fronts. Encapsule le noyau ; les fronts ne font que
/// de la présentation au-dessus.
#[derive(uniffi::Object)]
pub struct ChampiniumNode {
    inner: Node,
}

/// Ouvre (ou crée) un nœud sous `data_dir` : identité persistée + blocs, avec la
/// modération par défaut active (non désactivable).
#[uniffi::export(async_runtime = "tokio")]
pub async fn open_node(data_dir: String) -> Result<Arc<ChampiniumNode>, FfiError> {
    let inner = Node::open(&PathBuf::from(data_dir)).await?;
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
            .map(|e| FfiCatalogEntry {
                issuer: e.issuer.to_string(),
                seq: e.seq,
                cids: e.cids.iter().map(|c| c.to_string()).collect(),
            })
            .collect()
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl ChampiniumNode {
    /// Écoute sur `addr` (multiaddr) ; renvoie l'adresse effectivement liée.
    pub async fn listen(&self, addr: String) -> Result<String, FfiError> {
        let addr = addr
            .parse()
            .map_err(|e| FfiError::Failed(format!("multiaddr invalide: {e}")))?;
        Ok(self.inner.listen(addr).await?.to_string())
    }

    /// Se connecte à un pair `/ip4/.../tcp/.../p2p/<peerid>`.
    pub async fn connect(&self, peer: String) -> Result<(), FfiError> {
        let addr = peer
            .parse()
            .map_err(|e| FfiError::Failed(format!("multiaddr invalide: {e}")))?;
        self.inner.connect(addr).await?;
        Ok(())
    }

    /// Ingestion d'un média (ffmpeg → HLS). Renvoie le CID du manifeste.
    pub async fn ingest_file(&self, path: String) -> Result<String, FfiError> {
        let cid = self.inner.ingest_file(std::path::Path::new(&path)).await?;
        Ok(cid.to_string())
    }

    /// Publie un feed signé listant `cids`.
    pub async fn publish_feed(&self, cids: Vec<String>) -> Result<(), FfiError> {
        let parsed = parse_cids(&cids)?;
        self.inner.publish_feed(&parsed).await?;
        Ok(())
    }

    /// Souscrit à une denylist signée (JSON `champinium-denylist/v1`) : active une
    /// modération fédérée côté front (au-delà de la denylist par défaut). La
    /// signature est vérifiée ; les blocs déjà en cache que la liste couvre sont
    /// purgés. Renvoie le nombre de blocs purgés.
    pub async fn subscribe_denylist(&self, denylist_json: String) -> Result<u64, FfiError> {
        let dl = Denylist::from_json(&denylist_json)?;
        let purged = self.inner.subscribe_denylist(&dl).await?;
        Ok(purged as u64)
    }

    /// Récupère et reconstruit un HLS depuis un manifeste, dans `out_dir`.
    /// Renvoie le chemin du `index.m3u8` jouable.
    pub async fn fetch_hls(
        &self,
        manifest_cid: String,
        out_dir: String,
    ) -> Result<String, FfiError> {
        let cid: Cid = manifest_cid
            .parse()
            .map_err(|e| FfiError::Failed(format!("CID invalide: {e}")))?;
        let playlist = self
            .inner
            .fetch_hls(cid, std::path::Path::new(&out_dir))
            .await?;
        Ok(playlist.to_string_lossy().into_owned())
    }
}

fn parse_cids(cids: &[String]) -> Result<Vec<Cid>, FfiError> {
    cids.iter()
        .map(|c| {
            c.parse::<Cid>()
                .map_err(|e| FfiError::Failed(format!("CID invalide '{c}': {e}")))
        })
        .collect()
}
