//! Auto-réparation : un bloc local corrompu (crash pendant l'écriture, disque)
//! ne doit pas rendre le CID définitivement irrécupérable.
//!
//! Preuve attendue : B a mis un bloc en cache, son fichier local est corrompu ;
//! `get` détecte le défaut d'intégrité, retombe sur le réseau (A le détient
//! toujours) et **répare** le cache local.

use champinium_core::identity::load_or_generate;
use champinium_core::{Blockstore, Node};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corrupted_local_block_is_refetched_from_network() {
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
    node_b
        .add_address(node_a.peer_id(), addr_a.clone())
        .await
        .unwrap();
    node_b.dial(addr_a).await.unwrap();

    let payload = b"bloc qui survivra a une corruption locale".to_vec();
    let cid = node_a.add(&payload).await.unwrap();

    // B récupère et met en cache (retries le temps que la DHT converge).
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if node_b.get(cid).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    })
    .await
    .expect("le premier transfert doit aboutir");

    // Corruption du cache local de B sous le même nom de CID.
    std::fs::write(dir.path().join("b").join(cid.to_string()), b"corrompu").unwrap();

    // `get` doit détecter la corruption et retomber sur le réseau, pas
    // renvoyer IntegrityMismatch pour toujours.
    let received = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(data) = node_b.get(cid).await {
                break data;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    })
    .await
    .expect("le bloc corrompu doit être re-récupéré depuis le réseau");

    assert_eq!(
        received, payload,
        "le contenu re-téléchargé doit être le bon"
    );
    assert_eq!(
        node_b.blockstore().get(&cid).unwrap(),
        payload,
        "le cache local doit avoir été réparé"
    );
}
