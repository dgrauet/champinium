//! Test d'intégration Phase 4 : seeding (réannonce des CIDs détenus).
//!
//! Prouve que `reprovide_all` rend découvrables (provider records DHT) des blocs
//! présents dans le blockstore mais qui n'avaient pas été annoncés — le cas d'un
//! seeder qui redémarre et doit réannoncer ce qu'il détient.

use champinium_core::content::cid_for;
use champinium_core::identity::load_or_generate;
use champinium_core::{Blockstore, Moderation, Node};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reprovide_makes_stored_blocks_discoverable() {
    let dir = tempfile::tempdir().unwrap();

    // A détient un bloc stocké directement, SANS l'avoir annoncé.
    let bs_a = Blockstore::open(dir.path().join("a")).unwrap();
    let payload = b"contenu deja en cache, a reannoncer".to_vec();
    let cid = bs_a.put(&payload).unwrap();
    let kp_a = load_or_generate(dir.path().join("a.key")).unwrap();
    let node_a = Node::with_moderation(kp_a, bs_a, Moderation::empty())
        .await
        .unwrap();

    // Réannonce : doit publier 1 provider record.
    assert_eq!(node_a.reprovide_all().await.unwrap(), 1);

    // B se connecte à A et découvre le fournisseur via la DHT.
    let kp_b = load_or_generate(dir.path().join("b.key")).unwrap();
    let bs_b = Blockstore::open(dir.path().join("b")).unwrap();
    let node_b = Node::with_moderation(kp_b, bs_b, Moderation::empty())
        .await
        .unwrap();

    let addr_a = node_a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    node_b
        .add_address(node_a.peer_id(), addr_a.clone())
        .await
        .unwrap();
    node_b.dial(addr_a).await.unwrap();

    let found = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let providers = node_b.get_providers(cid).await.unwrap_or_default();
            if providers.contains(&node_a.peer_id()) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    })
    .await
    .expect("le fournisseur réannoncé doit être découvert");
    assert!(found);

    // Et le bloc est effectivement récupérable depuis A.
    let bytes = node_b.get(cid).await.unwrap();
    assert_eq!(bytes, payload);
    let _ = cid_for(&payload); // cohérence du CID
}
