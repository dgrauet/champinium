//! Catalogue décentralisé reconstruit localement.
//!
//! CRDT « maison » minimal : une map **last-writer-wins par émetteur**, la version
//! étant le `seq` du feed signé. Chaque nœud reconstruit son catalogue en écoutant
//! les feeds diffusés en gossipsub. Convergent : deux nœuds ayant reçu les mêmes
//! feeds (dans n'importe quel ordre) obtiennent le même catalogue (on garde, par
//! émetteur, le feed de `seq` le plus élevé, signature vérifiée).

use crate::error::Result as CoreResult;
use crate::feed::Feed;
use cid::Cid;
use libp2p::PeerId;
use std::collections::{HashMap, HashSet};

/// Une entrée de catalogue : le dernier feed connu d'un créateur.
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub issuer: PeerId,
    pub seq: u64,
    pub cids: Vec<Cid>,
}

/// Catalogue : dernier feed connu par émetteur.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    feeds: HashMap<PeerId, Feed>,
}

impl Catalog {
    /// Crée un catalogue vide.
    pub fn new() -> Self {
        Self::default()
    }

    /// Applique un feed reçu : **vérifie la signature**, puis le retient s'il est
    /// plus récent (seq strictement supérieur) que celui déjà connu pour cet
    /// émetteur. Renvoie `true` si le catalogue a changé.
    pub fn apply(&mut self, feed: Feed) -> CoreResult<bool> {
        feed.verify()?;
        let issuer = feed.issuer_peer_id()?;
        let newer = match self.feeds.get(&issuer) {
            Some(existing) => feed.seq > existing.seq,
            None => true,
        };
        if newer {
            self.feeds.insert(issuer, feed);
        }
        Ok(newer)
    }

    /// Entrées du catalogue (un feed courant par émetteur).
    pub fn entries(&self) -> Vec<CatalogEntry> {
        self.feeds
            .iter()
            .filter_map(|(issuer, feed)| {
                feed.cids().ok().map(|cids| CatalogEntry {
                    issuer: *issuer,
                    seq: feed.seq,
                    cids,
                })
            })
            .collect()
    }

    /// Tous les CIDs connus, tous émetteurs confondus.
    pub fn all_cids(&self) -> HashSet<Cid> {
        self.feeds
            .values()
            .filter_map(|f| f.cids().ok())
            .flatten()
            .collect()
    }

    /// Nombre d'émetteurs connus.
    pub fn issuer_count(&self) -> usize {
        self.feeds.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::cid_for;
    use libp2p::identity::Keypair;

    #[test]
    fn keeps_highest_seq_per_issuer() {
        let issuer = Keypair::generate_ed25519();
        let mut cat = Catalog::new();

        let v1 = Feed::build_signed(&issuer, 1, &[cid_for(b"a")]).unwrap();
        let v2 = Feed::build_signed(&issuer, 2, &[cid_for(b"a"), cid_for(b"b")]).unwrap();

        assert!(cat.apply(v2.clone()).unwrap());
        // Un feed plus ancien (seq inférieur) est ignoré.
        assert!(!cat.apply(v1).unwrap());
        assert_eq!(cat.all_cids().len(), 2);
        assert_eq!(cat.issuer_count(), 1);
    }

    #[test]
    fn convergence_is_order_independent() {
        let issuer = Keypair::generate_ed25519();
        let v1 = Feed::build_signed(&issuer, 1, &[cid_for(b"a")]).unwrap();
        let v2 = Feed::build_signed(&issuer, 2, &[cid_for(b"b")]).unwrap();

        let mut a = Catalog::new();
        a.apply(v1.clone()).unwrap();
        a.apply(v2.clone()).unwrap();

        let mut b = Catalog::new();
        b.apply(v2).unwrap();
        b.apply(v1).unwrap();

        assert_eq!(a.all_cids(), b.all_cids());
    }

    #[test]
    fn rejects_unsigned_or_tampered() {
        let issuer = Keypair::generate_ed25519();
        let mut feed = Feed::build_signed(&issuer, 1, &[cid_for(b"a")]).unwrap();
        feed.signature = None;
        let mut cat = Catalog::new();
        assert!(cat.apply(feed).is_err());
    }
}
