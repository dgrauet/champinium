//! Tests d'intégration de la modération (Phase 2 — garde-fou).
//!
//! Vérifie les deux checkpoints, et qu'un contenu NON matché circule normalement :
//! - #1 ingestion : refus de publier un contenu matché ;
//! - #2 réception/service : refus de récupérer et refus de servir un contenu matché.

use champinium_core::content::cid_for;
use champinium_core::identity::load_or_generate;
use champinium_core::{Blockstore, CoreError, Denylist, Moderation, Node};
use std::time::Duration;

const UPDATED: &str = "2026-06-24T00:00:00Z";

/// Construit un moteur de modération bloquant `cids`, via une denylist signée.
fn moderation_blocking(issuer_key: &std::path::Path, cids: &[cid::Cid]) -> Moderation {
    let issuer = load_or_generate(issuer_key).unwrap();
    let dl = Denylist::build_signed("test", UPDATED, &issuer, cids).unwrap();
    let mut m = Moderation::empty();
    m.subscribe(&dl).unwrap();
    m
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ingestion_checkpoint_refuses_blocked_content() {
    let dir = tempfile::tempdir().unwrap();
    let forbidden = b"contenu interdit a l'ingestion".to_vec();
    let bad_cid = cid_for(&forbidden);

    let moderation = moderation_blocking(&dir.path().join("issuer.key"), &[bad_cid]);
    let kp = load_or_generate(dir.path().join("node.key")).unwrap();
    let bs = Blockstore::open(dir.path().join("blocks")).unwrap();
    let node = Node::with_moderation(kp, bs, moderation).await.unwrap();

    let err = node.add(&forbidden).await.unwrap_err();
    assert!(
        matches!(err, CoreError::Moderated(_)),
        "add doit être refusé"
    );
    assert!(
        !node.blockstore().has(&bad_cid),
        "le contenu matché ne doit pas être stocké"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reception_refuses_blocked_but_allows_clean() {
    let dir = tempfile::tempdir().unwrap();
    let clean = b"contenu legitime".to_vec();
    let forbidden = b"contenu interdit a la reception".to_vec();
    let bad_cid = cid_for(&forbidden);

    // A : aucune modération, publie les deux contenus.
    let kp_a = load_or_generate(dir.path().join("a.key")).unwrap();
    let bs_a = Blockstore::open(dir.path().join("a")).unwrap();
    let node_a = Node::with_moderation(kp_a, bs_a, Moderation::empty())
        .await
        .unwrap();
    let clean_cid = node_a.add(&clean).await.unwrap();
    let bad_cid2 = node_a.add(&forbidden).await.unwrap();
    assert_eq!(bad_cid, bad_cid2);

    // B : bloque le mauvais CID.
    let moderation = moderation_blocking(&dir.path().join("issuer.key"), &[bad_cid]);
    let kp_b = load_or_generate(dir.path().join("b.key")).unwrap();
    let bs_b = Blockstore::open(dir.path().join("b")).unwrap();
    let node_b = Node::with_moderation(kp_b, bs_b, moderation).await.unwrap();

    let addr_a = node_a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    node_b
        .add_address(node_a.peer_id(), addr_a.clone())
        .await
        .unwrap();
    node_b.dial(addr_a).await.unwrap();

    // Contenu propre : circule normalement.
    let received = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(d) = node_b.get(clean_cid).await {
                break d;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    })
    .await
    .expect("le contenu propre doit être transféré");
    assert_eq!(received, clean);

    // Contenu matché : refusé immédiatement à la réception, jamais récupéré/caché.
    let err = node_b.get(bad_cid).await.unwrap_err();
    assert!(matches!(err, CoreError::Moderated(_)));
    assert!(
        !node_b.blockstore().has(&bad_cid),
        "le contenu matché ne doit pas être mis en cache"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serving_checkpoint_refuses_to_serve_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let forbidden = b"contenu interdit au service".to_vec();

    // A possède le bloc (stocké directement) mais sa modération en interdit le service.
    let bs_a = Blockstore::open(dir.path().join("a")).unwrap();
    let bad_cid = bs_a.put(&forbidden).unwrap();
    let moderation_a = moderation_blocking(&dir.path().join("issuer.key"), &[bad_cid]);
    let kp_a = load_or_generate(dir.path().join("a.key")).unwrap();
    let node_a = Node::with_moderation(kp_a, bs_a, moderation_a)
        .await
        .unwrap();
    node_a.provide(bad_cid).await.unwrap(); // annonce quand même le provider record

    // B : aucune modération — il essaie de récupérer.
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

    // Pendant une fenêtre, B ne doit JAMAIS réussir à obtenir le bloc (A refuse de servir).
    for _ in 0..20 {
        if node_b.get(bad_cid).await.is_ok() {
            panic!("A n'aurait jamais dû servir un contenu matché");
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert!(!node_b.blockstore().has(&bad_cid));
}
