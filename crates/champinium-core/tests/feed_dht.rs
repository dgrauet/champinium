//! Test d'intégration Phase 2/4 : découverte de feed via la DHT (hors gossip).
//!
//! A publie son feed AVANT que B ne se connecte : le gossip ne peut donc pas
//! l'avoir livré à B. B doit retrouver le feed par un GET dans la DHT.

use champinium_core::content::cid_for;
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
async fn feed_is_discoverable_via_dht_without_gossip() {
    let dir = tempfile::tempdir().unwrap();

    // A publie son feed (PUT DHT) avant toute connexion : pas de gossip vers B.
    let node_a = node(dir.path(), "a").await;
    node_a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    let c1 = cid_for(b"dht feed item 1");
    let c2 = cid_for(b"dht feed item 2");
    node_a.publish_feed(&[c1, c2]).await.unwrap();

    // B se connecte ensuite et récupère le feed via la DHT.
    let node_b = node(dir.path(), "b").await;
    let addr_a = node_a.listen_addrs().await.unwrap();
    let addr_a = addr_a.into_iter().next().expect("adresse d'écoute de A");
    node_b
        .add_address(node_a.peer_id(), addr_a.clone())
        .await
        .unwrap();
    node_b.dial(addr_a).await.unwrap();

    let feed = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(Some(feed)) = node_b.fetch_feed(node_a.peer_id()).await {
                return feed;
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
    })
    .await
    .expect("le feed doit être trouvé dans la DHT");

    let cids = feed.cids().unwrap();
    assert!(cids.contains(&c1) && cids.contains(&c2));
    // fetch_feed a aussi alimenté le catalogue local de B.
    assert!(node_b.catalog_cids().contains(&c1));
}
