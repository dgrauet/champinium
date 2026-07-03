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

/// Borne par défaut du nombre d'émetteurs retenus (anti-DoS : sans borne, des
/// feeds signés par des clés jetables feraient croître la mémoire sans limite).
pub const DEFAULT_MAX_ISSUERS: usize = 1024;

/// Catalogue : dernier feed connu par émetteur.
#[derive(Debug, Clone)]
pub struct Catalog {
    feeds: HashMap<PeerId, Feed>,
    max_issuers: usize,
}

impl Default for Catalog {
    fn default() -> Self {
        Self::with_max_issuers(DEFAULT_MAX_ISSUERS)
    }
}

impl Catalog {
    /// Crée un catalogue vide, borné à [`DEFAULT_MAX_ISSUERS`] émetteurs.
    pub fn new() -> Self {
        Self::default()
    }

    /// Crée un catalogue vide borné à `max` émetteurs.
    pub fn with_max_issuers(max: usize) -> Self {
        Self {
            feeds: HashMap::new(),
            max_issuers: max,
        }
    }

    /// Applique un feed reçu : **vérifie la signature**, puis le retient s'il est
    /// plus récent (seq strictement supérieur) que celui déjà connu pour cet
    /// émetteur. Un émetteur inconnu est refusé si le catalogue est plein
    /// (borne anti-DoS ; pas d'éviction, sinon des clés jetables pourraient
    /// chasser les feeds légitimes). Renvoie `true` si le catalogue a changé.
    pub fn apply(&mut self, feed: Feed) -> CoreResult<bool> {
        feed.verify()?;
        let issuer = feed.issuer_peer_id()?;
        let newer = match self.feeds.get(&issuer) {
            Some(existing) => feed.seq > existing.seq,
            None => self.feeds.len() < self.max_issuers,
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
    fn bounded_rejects_new_issuers_when_full() {
        let mut cat = Catalog::with_max_issuers(2);
        let k1 = Keypair::generate_ed25519();
        let k2 = Keypair::generate_ed25519();
        let k3 = Keypair::generate_ed25519();

        assert!(cat
            .apply(Feed::build_signed(&k1, 1, &[cid_for(b"a")]).unwrap())
            .unwrap());
        assert!(cat
            .apply(Feed::build_signed(&k2, 1, &[cid_for(b"b")]).unwrap())
            .unwrap());

        // Plein : un émetteur inconnu est refusé (pas d'éviction, sinon un
        // attaquant pourrait chasser les feeds légitimes).
        assert!(!cat
            .apply(Feed::build_signed(&k3, 1, &[cid_for(b"c")]).unwrap())
            .unwrap());
        assert_eq!(cat.issuer_count(), 2);

        // ... mais un émetteur déjà connu peut toujours se mettre à jour.
        assert!(cat
            .apply(Feed::build_signed(&k1, 2, &[cid_for(b"d")]).unwrap())
            .unwrap());
        assert_eq!(cat.issuer_count(), 2);
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
