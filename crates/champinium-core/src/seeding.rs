//! Index de seed proactif : publications retenues par channel souscrit, pins,
//! quota d'octets et ordre d'éviction.
//!
//! Module de **logique pure** — aucun accès réseau ici. `p2p.rs` branchera
//! cet index sur le suivi de channel (lot c) pour décider quoi seeder et quoi
//! évincer sous quota. La réplication (nombre de fournisseurs DHT) est fournie
//! par l'appelant à `eviction_order`, jamais calculée ici.

use crate::blockstore::Blockstore;
use crate::error::{CoreError, Result as CoreResult};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

/// Quota de seed par défaut (20 Gio) si aucun `.seed_quota` n'est persisté.
pub const DEFAULT_SEED_QUOTA_BYTES: u64 = 20 * 1024 * 1024 * 1024;

/// Une publication (manifeste HLS + segments) retenue pour le seed proactif.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeededPublication {
    pub manifest_cid: String,
    pub segment_cids: Vec<String>,
    pub total_bytes: u64,
    /// Rang monotone d'entrée dans l'index — sert d'ancienneté (assigné par
    /// `SeedIndex::insert`, pas par l'appelant).
    pub order: u64,
}

/// Index des publications retenues pour le seed proactif, par émetteur.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SeedIndex {
    publications: BTreeMap<String, Vec<SeededPublication>>,
    pins: BTreeSet<String>,
    next_order: u64,
}

impl SeedIndex {
    /// Insère une publication pour cet émetteur ; assigne `order` (ignore la
    /// valeur passée par l'appelant, l'index est seul garant de l'ancienneté).
    pub fn insert(&mut self, issuer: impl Into<String>, publication: SeededPublication) {
        let order = self.next_order;
        self.next_order += 1;
        let publication = SeededPublication {
            order,
            ..publication
        };
        self.publications
            .entry(issuer.into())
            .or_default()
            .push(publication);
    }

    /// Retire une publication par CID de manifeste, où qu'elle soit. Renvoie
    /// l'émetteur et la publication retirée si trouvée.
    pub fn remove_publication(
        &mut self,
        manifest_cid: &str,
    ) -> Option<(String, SeededPublication)> {
        for (issuer, pubs) in self.publications.iter_mut() {
            if let Some(pos) = pubs.iter().position(|p| p.manifest_cid == manifest_cid) {
                let removed = pubs.remove(pos);
                let issuer = issuer.clone();
                let empty = pubs.is_empty();
                if empty {
                    self.publications.remove(&issuer);
                }
                return Some((issuer, removed));
            }
        }
        None
    }

    /// Publications retenues pour un émetteur donné (vide si inconnu).
    pub fn publications_of(&self, issuer: &str) -> &[SeededPublication] {
        self.publications
            .get(issuer)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Indique si ce CID de manifeste est déjà retenu (tous émetteurs confondus).
    pub fn contains_manifest(&self, manifest_cid: &str) -> bool {
        self.publications
            .values()
            .any(|pubs| pubs.iter().any(|p| p.manifest_cid == manifest_cid))
    }

    /// Somme des `total_bytes` de toutes les publications retenues.
    pub fn total_bytes(&self) -> u64 {
        self.publications
            .values()
            .flatten()
            .map(|p| p.total_bytes)
            .sum()
    }

    /// Épingle un manifeste (exempté d'éviction). Renvoie `true` s'il ne
    /// l'était pas déjà (sémantique `BTreeSet::insert`).
    pub fn pin(&mut self, manifest_cid: &str) -> bool {
        self.pins.insert(manifest_cid.to_string())
    }

    /// Retire l'épinglage. Renvoie `true` s'il était épinglé.
    pub fn unpin(&mut self, manifest_cid: &str) -> bool {
        self.pins.remove(manifest_cid)
    }

    /// Indique si ce manifeste est épinglé.
    pub fn is_pinned(&self, manifest_cid: &str) -> bool {
        self.pins.contains(manifest_cid)
    }

    /// Tous les CIDs référencés par l'index (manifestes + segments), pour la
    /// suppression des blocs orphelins ou la réannonce.
    pub fn all_cids(&self) -> Vec<String> {
        let mut out = Vec::new();
        for pubs in self.publications.values() {
            for p in pubs {
                out.push(p.manifest_cid.clone());
                out.extend(p.segment_cids.iter().cloned());
            }
        }
        out
    }

    /// Purge toutes les publications d'un émetteur (ex. désabonnement).
    /// Si `keep_pinned`, les publications épinglées sont conservées dans
    /// l'index ; dans tous les cas, les publications évincées sont renvoyées
    /// pour que l'appelant supprime les blocs correspondants.
    pub fn purge_issuer(&mut self, issuer: &str, keep_pinned: bool) -> Vec<SeededPublication> {
        let Some(pubs) = self.publications.get(issuer) else {
            return Vec::new();
        };
        if keep_pinned {
            let (kept, evicted): (Vec<_>, Vec<_>) = pubs
                .iter()
                .cloned()
                .partition(|p| self.pins.contains(&p.manifest_cid));
            if kept.is_empty() {
                self.publications.remove(issuer);
            } else {
                self.publications.insert(issuer.to_string(), kept);
            }
            evicted
        } else {
            self.publications.remove(issuer).unwrap_or_default()
        }
    }
}

/// Ordre d'éviction des publications NON épinglées : réplication décroissante
/// (celles déjà bien répliquées ailleurs sur le réseau partent en premier),
/// puis `order` croissant (la plus ancienne d'abord) en cas d'égalité. Fonction
/// pure : la réplication est mesurée par l'appelant (ex. `replication_factor`
/// DHT), jamais ici.
pub fn eviction_order<'a>(
    index: &'a SeedIndex,
    replication: &HashMap<String, usize>,
) -> Vec<&'a SeededPublication> {
    let mut candidates: Vec<&SeededPublication> = index
        .publications
        .values()
        .flatten()
        .filter(|p| !index.is_pinned(&p.manifest_cid))
        .collect();
    candidates.sort_by(|a, b| {
        let ra = replication.get(&a.manifest_cid).copied().unwrap_or(0);
        let rb = replication.get(&b.manifest_cid).copied().unwrap_or(0);
        rb.cmp(&ra).then_with(|| a.order.cmp(&b.order))
    });
    candidates
}

/// Chemin de l'index de seed persisté (à côté des blocs).
fn seed_index_path(blockstore: &Blockstore) -> PathBuf {
    blockstore.root().join(".seed_index")
}

/// Charge l'index de seed persisté (défaut vide si absent/corrompu — ne doit
/// jamais empêcher le démarrage du nœud).
pub fn load_seed_index(blockstore: &Blockstore) -> SeedIndex {
    std::fs::read_to_string(seed_index_path(blockstore))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persiste l'index de seed (JSON).
pub fn save_seed_index(blockstore: &Blockstore, index: &SeedIndex) -> CoreResult<()> {
    let json = serde_json::to_string(index)
        .map_err(|e| CoreError::Network(format!("json index de seed: {e}")))?;
    std::fs::write(seed_index_path(blockstore), json)?;
    Ok(())
}

/// Chemin du quota de seed persisté (à côté des blocs).
fn seed_quota_path(blockstore: &Blockstore) -> PathBuf {
    blockstore.root().join(".seed_quota")
}

/// Charge le quota de seed persisté (`DEFAULT_SEED_QUOTA_BYTES` si
/// absent/corrompu).
pub fn load_seed_quota(blockstore: &Blockstore) -> u64 {
    std::fs::read_to_string(seed_quota_path(blockstore))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(DEFAULT_SEED_QUOTA_BYTES)
}

/// Persiste le quota de seed (octets, texte brut).
pub fn save_seed_quota(blockstore: &Blockstore, quota_bytes: u64) -> CoreResult<()> {
    std::fs::write(seed_quota_path(blockstore), quota_bytes.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn publication(manifest_cid: &str, total_bytes: u64) -> SeededPublication {
        SeededPublication {
            manifest_cid: manifest_cid.to_string(),
            segment_cids: vec![
                format!("{manifest_cid}-seg0"),
                format!("{manifest_cid}-seg1"),
            ],
            total_bytes,
            order: 0, // ignoré par `insert`, réassigné par l'index.
        }
    }

    fn store() -> (Blockstore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (Blockstore::open(dir.path()).unwrap(), dir)
    }

    #[test]
    fn insert_and_total_bytes() {
        let mut idx = SeedIndex::default();
        idx.insert("issuer-a", publication("m1", 100));
        idx.insert("issuer-a", publication("m2", 50));
        idx.insert("issuer-b", publication("m3", 25));
        assert_eq!(idx.total_bytes(), 175);
        assert_eq!(idx.publications_of("issuer-a").len(), 2);
        assert!(idx.contains_manifest("m3"));
        assert!(!idx.contains_manifest("absent"));
    }

    #[test]
    fn insert_assigns_monotone_order() {
        let mut idx = SeedIndex::default();
        idx.insert("issuer-a", publication("m1", 1));
        idx.insert("issuer-a", publication("m2", 1));
        let pubs = idx.publications_of("issuer-a");
        assert_eq!(pubs[0].order, 0);
        assert_eq!(pubs[1].order, 1);
    }

    #[test]
    fn json_roundtrip() {
        let mut idx = SeedIndex::default();
        idx.insert("issuer-a", publication("m1", 100));
        idx.pin("m1");
        let json = serde_json::to_string(&idx).unwrap();
        let restored: SeedIndex = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.total_bytes(), 100);
        assert!(restored.is_pinned("m1"));
        assert_eq!(restored.publications_of("issuer-a").len(), 1);
    }

    #[test]
    fn remove_publication_finds_and_deletes() {
        let mut idx = SeedIndex::default();
        idx.insert("issuer-a", publication("m1", 100));
        let (issuer, removed) = idx.remove_publication("m1").unwrap();
        assert_eq!(issuer, "issuer-a");
        assert_eq!(removed.manifest_cid, "m1");
        assert!(!idx.contains_manifest("m1"));
        assert!(idx.remove_publication("m1").is_none());
    }

    #[test]
    fn all_cids_includes_manifests_and_segments() {
        let mut idx = SeedIndex::default();
        idx.insert("issuer-a", publication("m1", 1));
        let cids = idx.all_cids();
        assert!(cids.contains(&"m1".to_string()));
        assert!(cids.contains(&"m1-seg0".to_string()));
        assert!(cids.contains(&"m1-seg1".to_string()));
    }

    #[test]
    fn eviction_order_prefers_higher_replication_first() {
        let mut idx = SeedIndex::default();
        idx.insert("issuer-a", publication("well-replicated", 1));
        idx.insert("issuer-a", publication("rare", 1));
        let mut replication = HashMap::new();
        replication.insert("well-replicated".to_string(), 5);
        replication.insert("rare".to_string(), 1);
        let order = eviction_order(&idx, &replication);
        assert_eq!(order[0].manifest_cid, "well-replicated");
        assert_eq!(order[1].manifest_cid, "rare");
    }

    #[test]
    fn eviction_order_breaks_ties_by_oldest_order_first() {
        let mut idx = SeedIndex::default();
        idx.insert("issuer-a", publication("older", 1));
        idx.insert("issuer-a", publication("newer", 1));
        let mut replication = HashMap::new();
        replication.insert("older".to_string(), 2);
        replication.insert("newer".to_string(), 2);
        let order = eviction_order(&idx, &replication);
        assert_eq!(order[0].manifest_cid, "older");
        assert_eq!(order[1].manifest_cid, "newer");
    }

    #[test]
    fn eviction_order_never_includes_pinned() {
        let mut idx = SeedIndex::default();
        idx.insert("issuer-a", publication("pinned", 1));
        idx.insert("issuer-a", publication("unpinned", 1));
        idx.pin("pinned");
        let mut replication = HashMap::new();
        replication.insert("pinned".to_string(), 10);
        replication.insert("unpinned".to_string(), 0);
        let order = eviction_order(&idx, &replication);
        assert_eq!(order.len(), 1);
        assert_eq!(order[0].manifest_cid, "unpinned");
    }

    #[test]
    fn purge_issuer_keeps_pinned_and_returns_rest() {
        let mut idx = SeedIndex::default();
        idx.insert("issuer-a", publication("pinned", 1));
        idx.insert("issuer-a", publication("unpinned", 1));
        idx.pin("pinned");
        let evicted = idx.purge_issuer("issuer-a", true);
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].manifest_cid, "unpinned");
        assert!(idx.contains_manifest("pinned"));
        assert!(!idx.contains_manifest("unpinned"));
    }

    #[test]
    fn purge_issuer_without_keep_pinned_removes_everything() {
        let mut idx = SeedIndex::default();
        idx.insert("issuer-a", publication("pinned", 1));
        idx.pin("pinned");
        let evicted = idx.purge_issuer("issuer-a", false);
        assert_eq!(evicted.len(), 1);
        assert!(!idx.contains_manifest("pinned"));
    }

    #[test]
    fn load_seed_index_defaults_when_absent() {
        let (bs, _d) = store();
        let idx = load_seed_index(&bs);
        assert_eq!(idx.total_bytes(), 0);
    }

    #[test]
    fn load_seed_index_tolerates_corruption() {
        let (bs, _d) = store();
        std::fs::write(bs.root().join(".seed_index"), b"{not valid json").unwrap();
        let idx = load_seed_index(&bs);
        assert_eq!(idx.total_bytes(), 0);
    }

    #[test]
    fn seed_index_save_load_roundtrip() {
        let (bs, _d) = store();
        let mut idx = SeedIndex::default();
        idx.insert("issuer-a", publication("m1", 42));
        save_seed_index(&bs, &idx).unwrap();
        let restored = load_seed_index(&bs);
        assert_eq!(restored.total_bytes(), 42);
    }

    #[test]
    fn load_seed_quota_defaults_when_absent() {
        let (bs, _d) = store();
        assert_eq!(load_seed_quota(&bs), DEFAULT_SEED_QUOTA_BYTES);
    }

    #[test]
    fn load_seed_quota_tolerates_corruption() {
        let (bs, _d) = store();
        std::fs::write(bs.root().join(".seed_quota"), b"not-a-number").unwrap();
        assert_eq!(load_seed_quota(&bs), DEFAULT_SEED_QUOTA_BYTES);
    }

    #[test]
    fn seed_quota_save_load_roundtrip() {
        let (bs, _d) = store();
        save_seed_quota(&bs, 12345).unwrap();
        assert_eq!(load_seed_quota(&bs), 12345);
    }
}
