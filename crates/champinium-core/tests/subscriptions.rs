//! Abonnements : liste locale persistée (spec channels §2). État PRIVÉ du
//! nœud — jamais publié sur le réseau.

use champinium_core::Node;
use libp2p::identity::Keypair;

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
