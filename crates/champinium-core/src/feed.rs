//! Feed mutable signé d'un créateur.
//!
//! Un feed est la liste, **signée par l'identité du créateur** (Ed25519), des CIDs
//! qu'il publie. Il est **versionné** par un `seq` monotone : à chaque mise à jour,
//! le créateur réémet un feed avec un `seq` plus grand. La résolution de conflit
//! est « le plus grand `seq` gagne » (last-writer-wins), la signature garantissant
//! l'authenticité. Diffusé en gossipsub (live) ; le catalogue est reconstruit en
//! écoutant (voir [`crate::catalog`]).

use crate::content::push_field;
use crate::error::{CoreError, Result as CoreResult};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use cid::Cid;
use libp2p::identity::{Keypair, PublicKey};
use libp2p::PeerId;
use serde::{Deserialize, Serialize};

/// Identifiant de schéma de feed.
pub const SCHEMA: &str = "champinium-feed/v1";

/// Feed signé d'un créateur (format `champinium-feed/v1`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Feed {
    /// Identifiant de schéma ; doit valoir [`SCHEMA`].
    pub schema: String,
    /// Clé publique Ed25519 de l'émetteur (protobuf libp2p, base64).
    pub issuer_pubkey: String,
    /// Numéro de version monotone (le plus grand gagne).
    pub seq: u64,
    /// CIDs publiés par le créateur (chaînes CIDv1).
    pub cids: Vec<String>,
    /// Signature Ed25519 (base64) sur les octets canoniques du feed.
    pub signature: Option<String>,
}

impl Feed {
    /// Octets canoniques signés (déterministes) : schéma, clé, seq, puis CIDs
    /// triés. Chaque champ est **préfixé par sa longueur** (non séparé par `\n`)
    /// pour éliminer toute malléabilité par décalage de frontière.
    fn signing_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        push_field(&mut buf, self.schema.as_bytes());
        push_field(&mut buf, self.issuer_pubkey.as_bytes());
        push_field(&mut buf, &self.seq.to_le_bytes());
        let mut cids = self.cids.clone();
        cids.sort();
        push_field(&mut buf, &(cids.len() as u64).to_le_bytes());
        for c in cids {
            push_field(&mut buf, c.as_bytes());
        }
        buf
    }

    /// Construit et **signe** un feed avec l'identité du créateur.
    pub fn build_signed(issuer: &Keypair, seq: u64, cids: &[Cid]) -> CoreResult<Self> {
        let mut feed = Self {
            schema: SCHEMA.to_string(),
            issuer_pubkey: B64.encode(issuer.public().encode_protobuf()),
            seq,
            cids: cids.iter().map(|c| c.to_string()).collect(),
            signature: None,
        };
        let sig = issuer
            .sign(&feed.signing_bytes())
            .map_err(|e| CoreError::Network(format!("signature feed: {e}")))?;
        feed.signature = Some(B64.encode(sig));
        Ok(feed)
    }

    /// Sérialise en JSON.
    pub fn to_json(&self) -> CoreResult<String> {
        serde_json::to_string(self).map_err(|e| CoreError::Network(format!("json feed: {e}")))
    }

    /// Parse depuis JSON.
    pub fn from_json(json: &[u8]) -> CoreResult<Self> {
        serde_json::from_slice(json).map_err(|e| CoreError::Network(format!("json feed: {e}")))
    }

    /// Clé publique décodée de l'émetteur.
    fn public_key(&self) -> CoreResult<PublicKey> {
        let bytes = B64
            .decode(&self.issuer_pubkey)
            .map_err(|e| CoreError::Network(format!("clé base64: {e}")))?;
        PublicKey::try_decode_protobuf(&bytes)
            .map_err(|e| CoreError::Network(format!("clé invalide: {e}")))
    }

    /// `PeerId` du créateur, dérivé de sa clé publique.
    pub fn issuer_peer_id(&self) -> CoreResult<PeerId> {
        Ok(self.public_key()?.to_peer_id())
    }

    /// Vérifie le schéma et la **signature** du feed.
    pub fn verify(&self) -> CoreResult<()> {
        if self.schema != SCHEMA {
            return Err(CoreError::Network(format!(
                "schéma feed inconnu: {}",
                self.schema
            )));
        }
        let sig_b64 = self
            .signature
            .as_ref()
            .ok_or_else(|| CoreError::Network("feed non signé".into()))?;
        let sig = B64
            .decode(sig_b64)
            .map_err(|e| CoreError::Network(format!("signature base64: {e}")))?;
        if self.public_key()?.verify(&self.signing_bytes(), &sig) {
            Ok(())
        } else {
            Err(CoreError::Network("signature feed invalide".into()))
        }
    }

    /// CIDs du feed (après parsing).
    pub fn cids(&self) -> CoreResult<Vec<Cid>> {
        self.cids
            .iter()
            .map(|c| c.parse::<Cid>().map_err(CoreError::Cid))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::cid_for;

    #[test]
    fn build_verify_roundtrip() {
        let issuer = Keypair::generate_ed25519();
        let feed = Feed::build_signed(&issuer, 1, &[cid_for(b"a"), cid_for(b"b")]).unwrap();
        let json = feed.to_json().unwrap();
        let parsed = Feed::from_json(json.as_bytes()).unwrap();
        parsed.verify().unwrap();
        assert_eq!(
            parsed.issuer_peer_id().unwrap(),
            issuer.public().to_peer_id()
        );
        assert_eq!(parsed.cids().unwrap().len(), 2);
    }

    #[test]
    fn tampered_feed_fails_verification() {
        let issuer = Keypair::generate_ed25519();
        let mut feed = Feed::build_signed(&issuer, 1, &[cid_for(b"a")]).unwrap();
        feed.cids.push(cid_for(b"injecte").to_string());
        assert!(feed.verify().is_err());
    }

    #[test]
    fn bumping_seq_requires_resigning() {
        let issuer = Keypair::generate_ed25519();
        let mut feed = Feed::build_signed(&issuer, 1, &[cid_for(b"a")]).unwrap();
        feed.seq = 2; // modifie le seq sans re-signer
        assert!(feed.verify().is_err());
    }
}
