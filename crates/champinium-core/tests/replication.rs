//! Test d'intégration : seed-what-you-consume → réplication.
//!
//! A publie un bloc ; B le consomme (donc le met en cache ET le réannonce) ;
//! A est ensuite mis hors ligne ; un troisième nœud C, connecté UNIQUEMENT à B,
//! récupère quand même le bloc — preuve que le consommateur B le reseede.

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

async fn fetch(node: &Node, cid: champinium_core::Cid) -> Vec<u8> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(b) = node.get(cid).await {
                return b;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    })
    .await
    .expect("récupération dans le délai imparti")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn consumer_reseeds_to_other_peers() {
    let dir = tempfile::tempdir().unwrap();
    let payload = b"contenu replique par seed-what-you-consume".to_vec();

    // A publie.
    let node_a = node(dir.path(), "a").await;
    let addr_a = node_a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    let cid = node_a.add(&payload).await.unwrap();

    // B consomme depuis A → met en cache ET réannonce.
    let node_b = node(dir.path(), "b").await;
    let addr_b = node_b
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    node_b
        .add_address(node_a.peer_id(), addr_a.clone())
        .await
        .unwrap();
    node_b.dial(addr_a).await.unwrap();
    assert_eq!(fetch(&node_b, cid).await, payload);
    assert!(node_b.blockstore().has(&cid));

    // A passe hors ligne.
    drop(node_a);
    tokio::time::sleep(Duration::from_millis(500)).await;

    // C, connecté UNIQUEMENT à B, récupère le bloc → il vient forcément de B.
    let node_c = node(dir.path(), "c").await;
    node_c
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    node_c
        .add_address(node_b.peer_id(), addr_b.clone())
        .await
        .unwrap();
    node_c.dial(addr_b).await.unwrap();
    assert_eq!(fetch(&node_c, cid).await, payload);
}
