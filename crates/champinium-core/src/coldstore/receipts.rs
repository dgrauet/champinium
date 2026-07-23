//! Reçus locaux d'archivage — dotfile `.archives`, patron de
//! `seeding::load_seed_index`/`save_seed_index` : JSON, corruption tolérée
//! (ne doit jamais empêcher le démarrage), purement informatif — l'index de
//! récupération réel, ce sont les tags on-chain, pas ce fichier.

use crate::coldstore::ArchiveReceipt;
use crate::error::{CoreError, Result as CoreResult};
use std::path::{Path, PathBuf};

/// Chemin du dotfile des reçus, dans le répertoire de données donné.
fn receipts_path(dir: &Path) -> PathBuf {
    dir.join(".archives")
}

/// Charge les reçus persistés (liste vide si absent ou corrompu).
pub fn load_receipts(dir: &Path) -> Vec<ArchiveReceipt> {
    std::fs::read_to_string(receipts_path(dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Ajoute un reçu et persiste la liste (JSON).
pub fn save_receipt(dir: &Path, receipt: &ArchiveReceipt) -> CoreResult<()> {
    let mut receipts = load_receipts(dir);
    receipts.push(receipt.clone());
    let json = serde_json::to_string(&receipts)
        .map_err(|e| CoreError::Network(format!("json reçus d'archive: {e}")))?;
    std::fs::write(receipts_path(dir), json)?;
    Ok(())
}
