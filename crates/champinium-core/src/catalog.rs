//! Catalogue décentralisé reconstruit localement.
//!
//! CRDT « maison » minimal : une map **last-writer-wins par émetteur**, la version
//! étant le `seq` du feed signé. Chaque nœud reconstruit son catalogue en écoutant
//! les feeds diffusés en gossipsub. Convergent : deux nœuds ayant reçu les mêmes
//! feeds (dans n'importe quel ordre) obtiennent le même catalogue (on garde, par
//! émetteur, le feed de `seq` le plus élevé, signature vérifiée).

use crate::error::Result as CoreResult;
use crate::feed::{ChannelMeta, Feed};
use cid::Cid;
use libp2p::PeerId;
use std::collections::{HashMap, HashSet};

/// Un contenu du catalogue avec ses métadonnées signées (titre, tags).
#[derive(Debug, Clone)]
pub struct CatalogItem {
    pub cid: Cid,
    pub title: String,
    pub tags: Vec<String>,
}

/// Une entrée de catalogue : le dernier feed connu d'un créateur.
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub issuer: PeerId,
    pub seq: u64,
    pub cids: Vec<Cid>,
    /// Contenus avec métadonnées (mêmes CIDs que `cids`, enrichis).
    pub items: Vec<CatalogItem>,
    /// Identité éditoriale du channel de cet émetteur (signée avec le feed).
    pub channel: ChannelMeta,
}

/// Un résultat de recherche locale.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub issuer: PeerId,
    pub cid: Cid,
    pub title: String,
    pub tags: Vec<String>,
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
    /// chasser les feeds légitimes) — **sauf** s'il figure dans `subscribed` :
    /// un abonnement local ne peut pas être évincé par du bruit réseau (spec
    /// channels §2). Renvoie `true` si le catalogue a changé.
    pub fn apply(&mut self, feed: Feed, subscribed: &HashSet<PeerId>) -> CoreResult<bool> {
        feed.verify()?;
        let issuer = feed.issuer_peer_id()?;
        let newer = match self.feeds.get(&issuer) {
            Some(existing) => feed.seq > existing.seq,
            None => subscribed.contains(&issuer) || self.feeds.len() < self.max_issuers,
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
                let cids = feed.cids().ok()?;
                let items = feed
                    .entries
                    .iter()
                    .filter_map(|e| {
                        e.cid.parse::<Cid>().ok().map(|cid| CatalogItem {
                            cid,
                            title: e.title.clone(),
                            tags: e.tags.clone(),
                        })
                    })
                    .collect();
                Some(CatalogEntry {
                    issuer: *issuer,
                    seq: feed.seq,
                    cids,
                    items,
                    channel: feed.channel.clone(),
                })
            })
            .collect()
    }

    /// Recherche locale : sous-chaîne insensible à la casse dans les titres,
    /// correspondance sur les tags (déjà normalisés en minuscules). Limite
    /// assumée (risque #4 du spec) : l'index ne couvre que les feeds que CE
    /// nœud a vus passer — il n'y a pas de recherche globale exhaustive sur un
    /// réseau décentralisé.
    pub fn search(&self, query: &str) -> Vec<SearchHit> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        let mut hits = Vec::new();
        for entry in self.entries() {
            for item in entry.items {
                if item.title.to_lowercase().contains(&needle)
                    || item.tags.iter().any(|t| t == &needle)
                {
                    hits.push(SearchHit {
                        issuer: entry.issuer,
                        cid: item.cid,
                        title: item.title,
                        tags: item.tags,
                    });
                }
            }
        }
        hits
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

    /// Aide de test : `apply` sans abonnement (le cas courant hors tâche 1).
    fn apply0(cat: &mut Catalog, feed: Feed) -> CoreResult<bool> {
        cat.apply(feed, &HashSet::new())
    }

    #[test]
    fn keeps_highest_seq_per_issuer() {
        let issuer = Keypair::generate_ed25519();
        let mut cat = Catalog::new();

        let v1 = Feed::build_signed(&issuer, 1, &[cid_for(b"a")]).unwrap();
        let v2 = Feed::build_signed(&issuer, 2, &[cid_for(b"a"), cid_for(b"b")]).unwrap();

        assert!(apply0(&mut cat, v2.clone()).unwrap());
        // Un feed plus ancien (seq inférieur) est ignoré.
        assert!(!apply0(&mut cat, v1).unwrap());
        assert_eq!(cat.all_cids().len(), 2);
        assert_eq!(cat.issuer_count(), 1);
    }

    #[test]
    fn convergence_is_order_independent() {
        let issuer = Keypair::generate_ed25519();
        let v1 = Feed::build_signed(&issuer, 1, &[cid_for(b"a")]).unwrap();
        let v2 = Feed::build_signed(&issuer, 2, &[cid_for(b"b")]).unwrap();

        let mut a = Catalog::new();
        apply0(&mut a, v1.clone()).unwrap();
        apply0(&mut a, v2.clone()).unwrap();

        let mut b = Catalog::new();
        apply0(&mut b, v2).unwrap();
        apply0(&mut b, v1).unwrap();

        assert_eq!(a.all_cids(), b.all_cids());
    }

    #[test]
    fn bounded_rejects_new_issuers_when_full() {
        let mut cat = Catalog::with_max_issuers(2);
        let k1 = Keypair::generate_ed25519();
        let k2 = Keypair::generate_ed25519();
        let k3 = Keypair::generate_ed25519();

        assert!(apply0(
            &mut cat,
            Feed::build_signed(&k1, 1, &[cid_for(b"a")]).unwrap()
        )
        .unwrap());
        assert!(apply0(
            &mut cat,
            Feed::build_signed(&k2, 1, &[cid_for(b"b")]).unwrap()
        )
        .unwrap());

        // Plein : un émetteur inconnu est refusé (pas d'éviction, sinon un
        // attaquant pourrait chasser les feeds légitimes).
        assert!(!apply0(
            &mut cat,
            Feed::build_signed(&k3, 1, &[cid_for(b"c")]).unwrap()
        )
        .unwrap());
        assert_eq!(cat.issuer_count(), 2);

        // ... mais un émetteur déjà connu peut toujours se mettre à jour.
        assert!(apply0(
            &mut cat,
            Feed::build_signed(&k1, 2, &[cid_for(b"d")]).unwrap()
        )
        .unwrap());
        assert_eq!(cat.issuer_count(), 2);
    }

    #[test]
    fn subscribed_issuer_bypasses_the_bound() {
        let mut cat = Catalog::with_max_issuers(1);
        let k1 = Keypair::generate_ed25519();
        let k2 = Keypair::generate_ed25519();
        let none = std::collections::HashSet::new();
        let subs: std::collections::HashSet<_> = [k2.public().to_peer_id()].into_iter().collect();

        assert!(cat
            .apply(Feed::build_signed(&k1, 1, &[cid_for(b"a")]).unwrap(), &none)
            .unwrap());
        // Plein : un inconnu non souscrit est refusé…
        let k3 = Keypair::generate_ed25519();
        assert!(!cat
            .apply(Feed::build_signed(&k3, 1, &[cid_for(b"c")]).unwrap(), &none)
            .unwrap());
        // …mais un émetteur SOUSCRIT est toujours admis (spec §2 : les
        // abonnements ne peuvent pas être évincés par le bruit du réseau).
        assert!(cat
            .apply(Feed::build_signed(&k2, 1, &[cid_for(b"b")]).unwrap(), &subs)
            .unwrap());
        assert_eq!(cat.issuer_count(), 2);
    }

    #[test]
    fn rejects_unsigned_or_tampered() {
        let issuer = Keypair::generate_ed25519();
        let mut feed = Feed::build_signed(&issuer, 1, &[cid_for(b"a")]).unwrap();
        feed.signature = None;
        let mut cat = Catalog::new();
        assert!(apply0(&mut cat, feed).is_err());
    }

    // --- Recherche locale (index reconstruit par écoute, limites assumées :
    // --- ne couvre que ce que CE nœud a vu passer) ---

    fn meta(cid: Cid, title: &str, tags: &[&str]) -> crate::feed::FeedEntry {
        crate::feed::FeedEntry {
            cid: cid.to_string(),
            title: title.to_string(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
        }
    }

    #[test]
    fn search_matches_title_and_tags_case_insensitive() {
        let k1 = Keypair::generate_ed25519();
        let k2 = Keypair::generate_ed25519();
        let mut cat = Catalog::new();
        apply0(
            &mut cat,
            Feed::build_signed_with(
                &k1,
                1,
                &ChannelMeta::default(),
                &[meta(cid_for(b"a"), "Aurores boréales", &["nature", "nuit"])],
            )
            .unwrap(),
        )
        .unwrap();
        apply0(
            &mut cat,
            Feed::build_signed_with(
                &k2,
                1,
                &ChannelMeta::default(),
                &[meta(cid_for(b"b"), "Recette de pâtes", &["cuisine"])],
            )
            .unwrap(),
        )
        .unwrap();

        // Titre, insensible à la casse.
        let hits = cat.search("AURORES");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].cid, cid_for(b"a"));
        assert_eq!(hits[0].title, "Aurores boréales");
        assert_eq!(hits[0].issuer, k1.public().to_peer_id());

        // Tag.
        let hits = cat.search("cuisine");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].cid, cid_for(b"b"));

        // Aucun résultat.
        assert!(cat.search("astronomie").is_empty());
    }

    #[test]
    fn entries_expose_items_with_metadata() {
        let k = Keypair::generate_ed25519();
        let mut cat = Catalog::new();
        apply0(
            &mut cat,
            Feed::build_signed_with(
                &k,
                1,
                &ChannelMeta::default(),
                &[meta(cid_for(b"a"), "Titre", &["tag"])],
            )
            .unwrap(),
        )
        .unwrap();
        let entries = cat.entries();
        assert_eq!(entries[0].items.len(), 1);
        assert_eq!(entries[0].items[0].title, "Titre");
        assert_eq!(entries[0].items[0].tags, vec!["tag"]);
        // `cids` reste dérivé (compat).
        assert_eq!(entries[0].cids, vec![cid_for(b"a")]);
    }

    #[test]
    fn entries_expose_channel_metadata() {
        let k = Keypair::generate_ed25519();
        let mut cat = Catalog::new();
        let ch = crate::feed::ChannelMeta {
            name: "Aurores".into(),
            description: "Ciels nocturnes".into(),
            avatar_cid: None,
        };
        apply0(
            &mut cat,
            Feed::build_signed_with(&k, 1, &ch, &[meta(cid_for(b"a"), "T", &[])]).unwrap(),
        )
        .unwrap();
        assert_eq!(cat.entries()[0].channel, ch);
    }
}
