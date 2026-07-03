//! Surface UniFFI exposée aux fronts natifs (contrat v3).
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

    /// Publie un feed signé listant `cids`.
    pub async fn publish_feed(&self, cids: Vec<String>) -> Result<(), FfiError> {
        let parsed = parse_cids(&cids)?;
        self.inner.publish_feed(&parsed).await?;
        Ok(())
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
        let cid: Cid = manifest_cid.parse().map_err(|e| FfiError::InvalidInput {
            msg: format!("CID invalide: {e}"),
        })?;
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
        node.publish_feed(vec![cid]).await.unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(5), rx)
            .await
            .expect("le listener doit être rappelé après publish_feed")
            .unwrap();
    }
}
