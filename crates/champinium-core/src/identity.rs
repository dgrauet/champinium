//! Identité cryptographique du nœud.
//!
//! Paire de clés **Ed25519** persistée localement → `PeerId` libp2p (et, plus
//! tard, DID). Tout contenu et tout feed sera signé avec cette identité ; la
//! réputation s'y attache. Phase 1 : génération, persistance, chargement.

use crate::error::{CoreError, Result};
use libp2p::identity::Keypair;
use libp2p::PeerId;
use std::path::Path;

/// Charge la clé depuis `path`, ou en génère une nouvelle (persistée) si absente.
pub fn load_or_generate(path: impl AsRef<Path>) -> Result<Keypair> {
    let path = path.as_ref();
    if path.exists() {
        let bytes = std::fs::read(path)?;
        Keypair::from_protobuf_encoding(&bytes)
            .map_err(|e| CoreError::Identity(format!("clé illisible: {e}")))
    } else {
        let kp = Keypair::generate_ed25519();
        let bytes = kp
            .to_protobuf_encoding()
            .map_err(|e| CoreError::Identity(format!("encodage: {e}")))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_private(path, &bytes)?;
        Ok(kp)
    }
}

/// Écrit un secret sur disque avec des permissions restreintes au propriétaire
/// (0600 sur Unix ; sur les autres OS, permissions par défaut).
#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes)?;
    Ok(())
}

/// `PeerId` dérivé d'une paire de clés.
pub fn peer_id(keypair: &Keypair) -> PeerId {
    PeerId::from(keypair.public())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_persists_and_reloads_same_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node.key");

        let first = load_or_generate(&path).unwrap();
        assert!(path.exists(), "la clé doit être persistée");

        let second = load_or_generate(&path).unwrap();
        assert_eq!(
            peer_id(&first),
            peer_id(&second),
            "recharger doit donner le même PeerId"
        );
    }

    #[test]
    fn distinct_paths_give_distinct_identities() {
        let dir = tempfile::tempdir().unwrap();
        let a = load_or_generate(dir.path().join("a.key")).unwrap();
        let b = load_or_generate(dir.path().join("b.key")).unwrap();
        assert_ne!(peer_id(&a), peer_id(&b));
    }

    #[cfg(unix)]
    #[test]
    fn generated_key_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node.key");
        load_or_generate(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "la clé privée ne doit être lisible que par son propriétaire"
        );
    }
}
