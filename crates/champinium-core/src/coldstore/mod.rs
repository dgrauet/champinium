//! Stockage froid optionnel (Arweave) — ADR 0008.
//!
//! Gaté par la feature cargo `cold-storage` (aucune dépendance tirée par
//! défaut). Ce module définit le trait `ColdStore` (repli de récupération et,
//! plus tard, archivage) et ses types associés ; le backend Arweave vit dans
//! [`arweave`], les reçus locaux dans [`receipts`]. Décision supply-chain :
//! voir `.superpowers/sdd/coldstore-decision.md` (hand-roll de la signature
//! Arweave à la Tâche 4, `reqwest` comme seul ajout HTTP).

pub mod arweave;
mod arweave_tx;
pub mod receipts;

use crate::error::{CoreError, Result as CoreResult};
use cid::Cid;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Contrat commun à tout backend de stockage froid (Arweave aujourd'hui,
/// Filecoin envisageable plus tard sans toucher au reste — voir la spec).
///
/// `retrieve`/`price`/`balance`/`archive` sont toutes implémentées par
/// [`arweave::ArweaveColdStore`]. `archive` signe (deep-hash + RSA-PSS aveuglé)
/// et téléverse une transaction Arweave **par item** (manifeste + chaque
/// segment), payée par le portefeuille du créateur.
#[async_trait::async_trait]
pub trait ColdStore: Send + Sync {
    /// Récupère les octets d'un CID depuis le filet d'archive, si présent.
    /// `Ok(None)` = introuvable (gateways muettes ou en échec) — jamais une
    /// erreur pour une simple absence, cf. spec « repli, pas un CDN ».
    async fn retrieve(&self, cid: Cid) -> CoreResult<Option<Vec<u8>>>;

    /// Archive une publication entière (manifeste + segments) sur le réseau
    /// froid, payé par le portefeuille du créateur. Tâche 4.
    async fn archive(
        &self,
        publication: &ArchivePayload,
        wallet: &ArweaveWallet,
    ) -> CoreResult<ArchiveReceipt>;

    /// Devis courant (winston, jamais mémorisé) pour archiver `bytes` octets.
    async fn price(&self, bytes: u64) -> CoreResult<u64>;

    /// Solde courant (winston) du portefeuille donné.
    async fn balance(&self, wallet: &ArweaveWallet) -> CoreResult<u64>;
}

/// Une publication à archiver : le manifeste HLS et tous ses segments,
/// chacun destiné à être étiqueté `champinium-cid: <cid>` côté transaction
/// Arweave (Tâche 4) — l'unité d'archivage est la même que celle du
/// SeedIndex et des pins (voir `seeding`).
#[derive(Debug, Clone)]
pub struct ArchivePayload {
    pub manifest_cid: Cid,
    /// (CID du bloc, octets du bloc) — manifeste inclus.
    pub items: Vec<(Cid, Vec<u8>)>,
}

/// Devis d'archivage calculé par [`crate::Node::archive_publication`] —
/// **n'envoie rien** : taille totale de la publication, coût estimé (winston,
/// somme des récompenses par transaction), solde courant du portefeuille, et
/// `sufficient` = le solde couvre-t-il le coût. L'opt-in en deux temps (devis
/// puis confirmation explicite) est imposé par la spec (ADR 0008) : aucun AR
/// n'est dépensé sans que l'utilisateur ait vu ce devis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveQuote {
    pub manifest_cid: String,
    pub bytes: u64,
    pub cost_winston: u64,
    pub balance_winston: u64,
    pub sufficient: bool,
}

/// Reçu local d'un archivage — purement informatif : l'index de récupération
/// réel, ce sont les tags on-chain (`champinium-cid`), pas ce fichier. Voir
/// [`receipts`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchiveReceipt {
    pub manifest_cid: String,
    pub tx_id: String,
    pub timestamp: u64,
    pub bytes: u64,
    pub cost_winston: u64,
}

/// Référence vers un portefeuille Arweave (fichier de clé JWK) fourni par
/// l'utilisateur — jamais créé, copié ou géré par champinium (« bring your
/// own wallet »). `from_path` ne fait QUE vérifier que le fichier existe et
/// que ses permissions sont restreintes au propriétaire ; le contenu n'est lu
/// qu'au moment d'un usage réel (dérivation d'adresse pour `balance`,
/// signature pour `archive` à la Tâche 4) — jamais copié ailleurs.
#[derive(Debug, Clone)]
pub struct ArweaveWallet {
    path: PathBuf,
}

impl ArweaveWallet {
    /// Référence un portefeuille existant. Refuse un chemin inexistant ou des
    /// permissions plus ouvertes que 0600 sur Unix (sur les autres OS, seule
    /// l'existence est vérifiée — pas d'équivalent direct de 0600).
    pub fn from_path(path: impl AsRef<Path>) -> CoreResult<Self> {
        let path = path.as_ref().to_path_buf();
        let metadata = std::fs::metadata(&path).map_err(|e| {
            CoreError::Identity(format!("portefeuille Arweave introuvable ({path:?}): {e}"))
        })?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = metadata.permissions().mode() & 0o777;
            if mode != 0o600 {
                return Err(CoreError::Identity(format!(
                    "portefeuille Arweave {path:?}: permissions {mode:o} trop ouvertes (attendu 0600)"
                )));
            }
        }
        #[cfg(not(unix))]
        {
            let _ = &metadata;
        }

        Ok(Self { path })
    }

    /// Chemin du fichier de clé référencé (jamais son contenu).
    pub fn path(&self) -> &Path {
        &self.path
    }
}
