//! Test : le `seq` de feed est persistant à travers les redémarrages.
//!
//! Sans persistance, un créateur qui redémarre republie un `seq` plus petit, que
//! le LWW des catalogues pairs ignore — ses mises à jour ne se propagent plus.

use champinium_core::content::cid_for;
use champinium_core::identity::load_or_generate;
use champinium_core::{Blockstore, Moderation, Node};

async fn open(dir: &std::path::Path) -> Node {
    let kp = load_or_generate(dir.join("node.key")).unwrap();
    let bs = Blockstore::open(dir.join("blocks")).unwrap();
    Node::with_moderation(kp, bs, Moderation::empty())
        .await
        .unwrap()
}

/// Le seq du feed d'un créateur reprend après recréation du nœud (même data dir).
fn self_seq(node: &Node) -> u64 {
    node.catalog_entries()
        .into_iter()
        .find(|e| e.issuer == node.peer_id())
        .map(|e| e.seq)
        .expect("le créateur figure dans son propre catalogue")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feed_seq_persists_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let c = cid_for(b"x");

    // Première session : deux publications → seq 1 puis 2.
    {
        let node = open(dir.path()).await;
        node.publish_feed(&[c]).await.unwrap();
        node.publish_feed(&[c]).await.unwrap();
        assert_eq!(self_seq(&node), 2);
    }

    // Nouvelle session (même répertoire) : le seq doit CONTINUER, pas repartir à 1.
    {
        let node = open(dir.path()).await;
        node.publish_feed(&[c]).await.unwrap();
        assert_eq!(
            self_seq(&node),
            3,
            "le seq doit reprendre à 3, pas redémarrer à 1"
        );
    }
}
