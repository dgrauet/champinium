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

/// Le facteur de réplication d'un CID est mesurable : après qu'un consommateur
/// a récupéré (et donc réannoncé) un bloc, la DHT compte DEUX fournisseurs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replication_factor_counts_providers() {
    let dir = tempfile::tempdir().unwrap();
    let node_a = node(dir.path(), "rf-a").await;
    let addr_a = node_a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    let payload = b"bloc replique mesurable".to_vec();
    let cid = node_a.add(&payload).await.unwrap();

    // Le publieur seul : facteur 1.
    assert_eq!(node_a.replication_factor(cid).await.unwrap(), 1);

    // B consomme → devient fournisseur → facteur 2 (vu de B).
    let node_b = node(dir.path(), "rf-b").await;
    node_b
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    node_b
        .add_address(node_a.peer_id(), addr_a.clone())
        .await
        .unwrap();
    node_b.dial(addr_a).await.unwrap();
    assert_eq!(fetch(&node_b, cid).await, payload);

    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if node_b.replication_factor(cid).await.unwrap() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    })
    .await
    .expect("le facteur de réplication doit atteindre 2 après le reseed");
}

/// Réplication OPPORTUNISTE (au-delà de seed-what-you-consume) : un nœud qui
/// connaît un contenu par son catalogue (feed gossip) mais ne l'a PAS consommé
/// le réplique proactivement s'il est sous-répliqué — manifeste HLS ET
/// segments — et devient fournisseur.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn under_provided_content_is_replicated_opportunistically() {
    use champinium_core::ingest::{HlsManifest, HlsSegment};

    let dir = tempfile::tempdir().unwrap();
    let node_a = node(dir.path(), "op-a").await;
    let addr_a = node_a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();

    // A publie un « contenu » : un segment + un manifeste HLS qui le référence.
    let seg = node_a.add(b"segment video opportuniste").await.unwrap();
    let manifest = HlsManifest::new(
        4.0,
        vec![HlsSegment {
            cid: seg.to_string(),
            duration: 4.0,
        }],
    );
    let manifest_cid = node_a
        .add(manifest.to_json().unwrap().as_bytes())
        .await
        .unwrap();

    // B apprend l'existence du contenu par le feed (catalogue), sans le consommer.
    let node_b = node(dir.path(), "op-b").await;
    node_b
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    node_b
        .add_address(node_a.peer_id(), addr_a.clone())
        .await
        .unwrap();
    node_b.dial(addr_a).await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            node_a.publish_feed(&[manifest_cid]).await.unwrap();
            if node_b.catalog_cids().contains(&manifest_cid) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .expect("le feed doit atteindre B");
    assert!(
        !node_b.blockstore().has(&manifest_cid),
        "pas encore consommé"
    );

    // Facteur observé : 1 (A seul) < cible 2 → B doit répliquer.
    let replicated = node_b.replicate_under_provided(2, 10).await.unwrap();
    assert_eq!(replicated, 1, "un contenu répliqué");
    assert!(node_b.blockstore().has(&manifest_cid), "manifeste répliqué");
    assert!(node_b.blockstore().has(&seg), "segments répliqués aussi");

    // Une seconde passe ne refait rien : le contenu est désormais local.
    assert_eq!(node_b.replicate_under_provided(2, 10).await.unwrap(), 0);
}
