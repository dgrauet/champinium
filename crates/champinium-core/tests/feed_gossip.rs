//! Test d'intégration Phase 2 : feeds signés diffusés en gossipsub.
//!
//! Preuve : le nœud A publie un feed signé ; le nœud B, en écoutant gossipsub,
//! reconstruit localement un catalogue contenant les CIDs annoncés par A.

use champinium_core::identity::load_or_generate;
use champinium_core::{Blockstore, Moderation, Node};
use std::time::Duration;

async fn node(dir: &std::path::Path, name: &str) -> Node {
    let kp = load_or_generate(dir.join(format!("{name}.key"))).unwrap();
    let bs = Blockstore::open(dir.join(name)).unwrap();
    Node::with_moderation(kp, bs, Moderation::empty())
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_feed_propagates_via_gossipsub() {
    let dir = tempfile::tempdir().unwrap();
    let node_a = node(dir.path(), "a").await;
    let node_b = node(dir.path(), "b").await;

    let addr_a = node_a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    node_b
        .add_address(node_a.peer_id(), addr_a.clone())
        .await
        .unwrap();
    node_b.dial(addr_a).await.unwrap();

    // A publie deux contenus puis annonce son feed.
    let c1 = node_a.add(b"feed item 1").await.unwrap();
    let c2 = node_a.add(b"feed item 2").await.unwrap();

    // B reconstruit son catalogue en écoutant. On republie tant que le mesh
    // gossipsub n'est pas formé (heartbeat ~1 s).
    let ok = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            node_a.publish_feed(&[c1, c2]).await.unwrap();
            let cids = node_b.catalog_cids();
            if cids.contains(&c1) && cids.contains(&c2) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .expect("le feed doit se propager via gossipsub");
    assert!(ok);

    // B connaît un émetteur (A) dans son catalogue reconstruit.
    let entries = node_b.catalog_entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].issuer, node_a.peer_id());
    assert!(entries[0].cids.contains(&c1));
}
