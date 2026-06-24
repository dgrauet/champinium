//! Stockage local content-addressed.
//!
//! Squelette Phase 1 : un bloc = un fichier nommé d'après son CID, sous un
//! répertoire racine. Suffisant pour `add`/`get` et le reseed. Le cache LRU et
//! la segmentation HLS viendront aux phases suivantes.

use crate::content::{cid_for, verify};
use crate::error::{CoreError, Result};
use cid::Cid;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Magasin de blocs sur disque, partageable entre tâches (`Arc`).
#[derive(Debug, Clone)]
pub struct Blockstore {
    root: Arc<PathBuf>,
}

impl Blockstore {
    /// Ouvre (ou crée) un magasin sous `root`.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        std::fs::create_dir_all(&root)?;
        Ok(Self {
            root: Arc::new(root),
        })
    }

    fn path_for(&self, cid: &Cid) -> PathBuf {
        self.root.join(cid.to_string())
    }

    /// Stocke un bloc et renvoie son CID (calculé sur le contenu).
    pub fn put(&self, bytes: &[u8]) -> Result<Cid> {
        let cid = cid_for(bytes);
        let path = self.path_for(&cid);
        // Content-addressed : si déjà présent, rien à réécrire (dédup).
        if !path.exists() {
            std::fs::write(&path, bytes)?;
        }
        Ok(cid)
    }

    /// Récupère un bloc par CID, en **vérifiant l'intégrité** au passage.
    pub fn get(&self, cid: &Cid) -> Result<Vec<u8>> {
        let path = self.path_for(cid);
        if !path.exists() {
            return Err(CoreError::BlockNotFound(cid.to_string()));
        }
        let bytes = std::fs::read(&path)?;
        if !verify(cid, &bytes) {
            return Err(CoreError::IntegrityMismatch);
        }
        Ok(bytes)
    }

    /// Indique si le bloc est présent localement.
    pub fn has(&self, cid: &Cid) -> bool {
        self.path_for(cid).exists()
    }

    /// CIDs actuellement détenus (pour réannoncer les provider records).
    pub fn list(&self) -> Result<Vec<Cid>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(self.root.as_path())? {
            let name = entry?.file_name();
            if let Some(s) = name.to_str() {
                if let Ok(cid) = s.parse::<Cid>() {
                    out.push(cid);
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (Blockstore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (Blockstore::open(dir.path()).unwrap(), dir)
    }

    #[test]
    fn put_then_get_roundtrips() {
        let (bs, _d) = store();
        let cid = bs.put(b"hello champinium").unwrap();
        assert_eq!(bs.get(&cid).unwrap(), b"hello champinium");
    }

    #[test]
    fn put_is_content_addressed_and_dedups() {
        let (bs, _d) = store();
        let a = bs.put(b"same").unwrap();
        let b = bs.put(b"same").unwrap();
        assert_eq!(a, b);
        assert_eq!(bs.list().unwrap().len(), 1);
    }

    #[test]
    fn get_missing_errors() {
        let (bs, _d) = store();
        let cid = crate::content::cid_for(b"absent");
        assert!(matches!(bs.get(&cid), Err(CoreError::BlockNotFound(_))));
    }

    #[test]
    fn corrupted_block_fails_integrity() {
        let (bs, _d) = store();
        let cid = bs.put(b"trusted").unwrap();
        // Corrompt le fichier sur disque sous le même nom de CID.
        std::fs::write(bs.path_for(&cid), b"evil").unwrap();
        assert!(matches!(bs.get(&cid), Err(CoreError::IntegrityMismatch)));
    }

    #[test]
    fn list_returns_stored_cids() {
        let (bs, _d) = store();
        let c1 = bs.put(b"one").unwrap();
        let c2 = bs.put(b"two").unwrap();
        let mut listed = bs.list().unwrap();
        listed.sort();
        let mut want = vec![c1, c2];
        want.sort();
        assert_eq!(listed, want);
    }
}
