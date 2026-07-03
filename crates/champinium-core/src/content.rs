//! Adressage par contenu (CID) — compatible IPFS.
//!
//! Convention Champinium : CIDv1, codec `raw` (0x55), multihash **sha2-256**.
//! Un même contenu produit toujours le même CID (dédup + intégrité).

use cid::Cid;
use multihash::Multihash;
use sha2::{Digest, Sha256};

/// Codec multicodec `raw` (octets bruts) — <https://github.com/multiformats/multicodec>.
const RAW_CODEC: u64 = 0x55;
/// Code multihash sha2-256.
const SHA2_256: u64 = 0x12;

/// Calcule le CID (v1, raw, sha2-256) d'un bloc d'octets.
pub fn cid_for(bytes: &[u8]) -> Cid {
    let digest = Sha256::digest(bytes);
    let mh =
        Multihash::<64>::wrap(SHA2_256, &digest).expect("digest sha2-256 (32o) tient dans 64o");
    Cid::new_v1(RAW_CODEC, mh)
}

/// Vérifie qu'un bloc correspond bien à un CID (intégrité).
pub fn verify(cid: &Cid, bytes: &[u8]) -> bool {
    &cid_for(bytes) == cid
}

/// Ajoute un champ à un tampon d'octets à signer en le **préfixant par sa
/// longueur** (u64 little-endian). Encodage non ambigu : deux découpages de
/// champs différents ne peuvent pas produire les mêmes octets, ce qui élimine
/// la malléabilité par décalage de frontière (voir `feed`/`moderation`).
pub(crate) fn push_field(buf: &mut Vec<u8>, field: &[u8]) {
    buf.extend_from_slice(&(field.len() as u64).to_le_bytes());
    buf.extend_from_slice(field);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cid_is_deterministic() {
        let a = cid_for(b"champinium");
        let b = cid_for(b"champinium");
        assert_eq!(a, b);
    }

    #[test]
    fn different_content_different_cid() {
        assert_ne!(cid_for(b"a"), cid_for(b"b"));
    }

    #[test]
    fn cidv1_raw_roundtrips_as_string() {
        let cid = cid_for(b"hello");
        let parsed: Cid = cid.to_string().parse().unwrap();
        assert_eq!(cid, parsed);
        assert_eq!(cid.codec(), RAW_CODEC);
        assert_eq!(cid.version(), cid::Version::V1);
    }

    #[test]
    fn verify_detects_tampering() {
        let cid = cid_for(b"original");
        assert!(verify(&cid, b"original"));
        assert!(!verify(&cid, b"tampered"));
    }
}
