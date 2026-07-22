//! Abonnements : liste locale persistée (spec channels §2). État PRIVÉ du
//! nœud — jamais publié sur le réseau.

use champinium_core::content::cid_for;
use champinium_core::identity::load_or_generate;
use champinium_core::{Blockstore, Moderation, Node};
use libp2p::identity::Keypair;
use std::time::Duration;

async fn node(dir: &std::path::Path, name: &str) -> Node {
    let kp = load_or_generate(dir.join(format!("{name}.key"))).unwrap();
    let bs = Blockstore::open(dir.join(name)).unwrap();
    Node::with_moderation(kp, bs, Moderation::empty())
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscriptions_persist_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let issuer = Keypair::generate_ed25519().public().to_peer_id();
    {
        let node = Node::open(dir.path()).await.unwrap();
        node.subscribe(issuer).unwrap();
        assert_eq!(node.subscriptions(), vec![issuer]);
    }
    let node = Node::open(dir.path()).await.unwrap();
    assert_eq!(node.subscriptions(), vec![issuer]);

    node.unsubscribe(issuer).unwrap();
    assert!(node.subscriptions().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_subscribed_filters_to_followed_issuers() {
    // Deux feeds dans le catalogue (via gossip local publish + apply direct) ;
    // seul l'émetteur souscrit apparaît dans catalog_subscribed().
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(dir.path()).await.unwrap();

    // Mon propre feed (non souscrit) + un feed tiers appliqué à la main.
    let cid = champinium_core::content::cid_for(b"x");
    node.publish_feed(&[cid]).await.unwrap();

    let other = Keypair::generate_ed25519();
    let feed = champinium_core::Feed::build_signed(&other, 1, &[cid]).unwrap();
    node.apply_feed_for_tests(feed).unwrap(); // voir note step 5

    node.subscribe(other.public().to_peer_id()).unwrap();
    let subbed = node.catalog_subscribed();
    assert_eq!(subbed.len(), 1);
    assert_eq!(subbed[0].issuer, other.public().to_peer_id());
    assert_eq!(node.catalog_entries().len(), 2, "Explorer voit tout");
}

/// Suivi actif (tâche 2) : B se souscrit à A sans qu'aucun gossip n'ait
/// circulé (A publie AVANT que B ne se connecte, comme dans
/// `feed_dht.rs::feed_is_discoverable_via_dht_without_gossip`) — le fetch
/// immédiat déclenché par `subscribe()` doit retrouver le feed via la DHT.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_triggers_immediate_dht_fetch() {
    let dir = tempfile::tempdir().unwrap();

    // A publie son feed (PUT DHT) avant toute connexion : pas de gossip vers B.
    let node_a = node(dir.path(), "a").await;
    node_a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    let c1 = cid_for(b"follow item 1");
    node_a.publish_feed(&[c1]).await.unwrap();

    // B se connecte ensuite (toujours pas de gossip reçu pour ce feed déjà
    // publié) puis se souscrit à A.
    let node_b = node(dir.path(), "b").await;
    let addr_a = node_a.listen_addrs().await.unwrap();
    let addr_a = addr_a.into_iter().next().expect("adresse d'écoute de A");
    node_b
        .add_address(node_a.peer_id(), addr_a.clone())
        .await
        .unwrap();
    node_b.dial(addr_a).await.unwrap();

    node_b.subscribe(node_a.peer_id()).unwrap();

    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let subbed = node_b.catalog_subscribed();
            if subbed.iter().any(|e| e.issuer == node_a.peer_id()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("le fetch immédiat à l'abonnement doit retrouver le feed de A");
}
