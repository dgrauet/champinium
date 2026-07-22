//! Test d'intégration : retrait de seed-what-you-consume.
//!
//! Nouveau contrat (spec channels lot c) : `get` (politique par défaut
//! `Stream`) ne met plus le bloc consommé en cache et n'annonce plus le
//! consommateur comme fournisseur. A publie un bloc ; B le consomme via
//! `get` ; ni le blockstore de B ni le facteur de réplication (mesuré depuis
//! A) ne doivent bouger — preuve directe et déterministe que B ne reseede
//! plus par défaut (on évite ici de faire dépendre le test du timing d'un
//! arrêt de processus, comme le faisait l'ancien test via un 3ᵉ nœud C
//! connecté à B après avoir mis A hors ligne).
//!
//! (Le contraire — `get_with(Seed)` reproduit l'ancien comportement — est
//! testé dans le module interne `p2p::tests`, `StorePolicy` étant
//! crate-interne. `replicate_under_provided` est supprimé avec ce lot : la
//! réplication opportuniste au-delà du défaut sera reprise sur des bases
//! explicites par un lot ultérieur.)

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
async fn consumer_does_not_reseed_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let payload = b"contenu simplement consomme, plus reseede".to_vec();

    // A publie.
    let node_a = node(dir.path(), "a").await;
    let addr_a = node_a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    let cid = node_a.add(&payload).await.unwrap();

    // B consomme via `get` (Stream) : ni cache, ni annonce.
    let node_b = node(dir.path(), "b").await;
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
    assert!(
        !node_b.blockstore().has(&cid),
        "Stream ne doit pas mettre le bloc en cache chez B"
    );

    // Laisse le temps à une éventuelle (mauvaise) réannonce, puis vérifie que
    // le facteur de réplication n'a pas bougé : B n'est PAS devenu fournisseur.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        node_a.replication_factor(cid).await.unwrap(),
        1,
        "le facteur de réplication ne doit pas monter après un `get` simple"
    );
    let providers = node_a.get_providers(cid).await.unwrap();
    assert!(
        !providers.contains(&node_b.peer_id()),
        "B ne doit pas apparaître comme fournisseur après un `get` simple"
    );
}
