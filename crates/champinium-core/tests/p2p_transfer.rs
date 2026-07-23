//! Test d'intégration Phase 1 : deux nœuds en mémoire (loopback).
//!
//! Preuve attendue : le nœud A publie un bloc (CID + provider record), le nœud B
//! le **découvre via la DHT** puis le **télécharge** et vérifie son intégrité —
//! transfert P2P brut de bout en bout entre deux nœuds. `get` (politique par
//! défaut `Stream`, depuis le retrait de seed-what-you-consume) ne met PAS le
//! bloc en cache chez B.

use champinium_core::identity::load_or_generate;
use champinium_core::{Blockstore, Node};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_nodes_provide_discover_and_transfer() {
    let dir = tempfile::tempdir().unwrap();

    let kp_a = load_or_generate(dir.path().join("a.key")).unwrap();
    let kp_b = load_or_generate(dir.path().join("b.key")).unwrap();
    let bs_a = Blockstore::open(dir.path().join("a")).unwrap();
    let bs_b = Blockstore::open(dir.path().join("b")).unwrap();

    let node_a = Node::new(kp_a, bs_a).await.unwrap();
    let node_b = Node::new(kp_b, bs_b).await.unwrap();

    let addr_a = node_a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();

    // B apprend A et s'y connecte ; identify peuplera la table de routage Kademlia.
    node_b
        .add_address(node_a.peer_id(), addr_a.clone())
        .await
        .unwrap();
    node_b.dial(addr_a).await.unwrap();

    // A publie un bloc : stockage local + annonce provider record dans la DHT.
    let payload = b"contenu transfere en P2P entre deux noeuds".to_vec();
    let cid = node_a.add(&payload).await.unwrap();

    // B le découvre via la DHT puis le télécharge. Retries le temps que
    // identify + Kademlia convergent (timing réseau, même en loopback).
    let received = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(data) = node_b.get(cid).await {
                break data;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    })
    .await
    .expect("le transfert P2P doit aboutir dans le délai imparti");

    assert_eq!(received, payload, "le bloc reçu doit être identique");
    assert!(
        !node_b.blockstore().has(&cid),
        "retrait de seed-what-you-consume : `get` (Stream) ne met plus le bloc en cache"
    );
}
