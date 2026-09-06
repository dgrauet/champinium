//! Listes de modération distribuées (spec 2026-09-06, ADR 0011).
use champinium_core::identity::load_or_generate;
use champinium_core::{Blockstore, Denylist, Feed, Moderation, Node};
use libp2p::identity::Keypair;
use libp2p::PeerId;
use std::time::Duration;

async fn node(dir: &std::path::Path, name: &str) -> Node {
    let kp = load_or_generate(dir.join(format!("{name}.key"))).unwrap();
    let bs = Blockstore::open(dir.join(name)).unwrap();
    Node::with_moderation_and_follow_interval(
        kp,
        bs,
        Moderation::empty(),
        Duration::from_millis(500),
    )
    .await
    .unwrap()
}

async fn poll_until<F: Fn() -> bool>(deadline: Duration, cond: F) {
    tokio::time::timeout(deadline, async {
        loop {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("condition attendue non atteinte");
}

fn list(editor: &Keypair, seq: u64, keys: &[PeerId]) -> Denylist {
    Denylist::build_signed("liste test", "2026-09-06T00:00:00Z", editor, seq, &[], keys).unwrap()
}

/// A (éditeur) publie une liste bannissant C ; B suit A → C disparaît du
/// catalogue de B et ses blocs sont purgés ; A lève le ban (seq+1) → B accepte
/// à nouveau les feeds de C ; B redémarré HORS LIGNE reste protégé (cache).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn published_denylist_reaches_follower_and_survives_offline_restart() {
    let dir = tempfile::tempdir().unwrap();
    let editor = Keypair::generate_ed25519();
    let a = node(dir.path(), "a").await; // transporte la liste
    let addr_a = a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    let kp_c = Keypair::generate_ed25519();
    let c_id = kp_c.public().to_peer_id();
    let b = node(dir.path(), "b").await;
    b.add_address(a.peer_id(), addr_a.clone()).await.unwrap();
    b.dial(addr_a.clone()).await.unwrap();

    // B connaît un contenu de C, en cache.
    let cid = b.add(b"contenu de c").await.unwrap();
    b.apply_feed_for_tests(Feed::build_signed(&kp_c, 1, &[cid]).unwrap())
        .unwrap();
    assert!(b.catalog_entries().iter().any(|e| e.issuer == c_id));

    // A publie la liste de l'éditeur (signée hors nœud), B suit l'éditeur.
    a.publish_denylist(&list(&editor, 1, &[c_id]))
        .await
        .unwrap();
    let editor_id = editor.public().to_peer_id();
    b.subscribe_denylist_issuer(editor_id).unwrap();
    poll_until(Duration::from_secs(30), || {
        b.denylist_source(&editor_id).map(|s| s.seq) == Some(1)
    })
    .await;
    poll_until(Duration::from_secs(30), || {
        !b.catalog_entries().iter().any(|e| e.issuer == c_id)
    })
    .await;
    assert!(!b.blockstore().has(&cid), "blocs de C purgés");
    assert!(
        b.apply_feed_for_tests(Feed::build_signed(&kp_c, 2, &[cid]).unwrap())
            .is_err()
            || !b.catalog_entries().iter().any(|e| e.issuer == c_id),
        "C refusé au catalogue"
    );

    // Levée du ban : seq 2 sans clé.
    a.publish_denylist(&list(&editor, 2, &[])).await.unwrap();
    poll_until(Duration::from_secs(30), || {
        b.denylist_source(&editor_id).map(|s| s.seq) == Some(2)
    })
    .await;
    b.apply_feed_for_tests(Feed::build_signed(&kp_c, 3, &[cid]).unwrap())
        .unwrap();
    assert!(
        b.catalog_entries().iter().any(|e| e.issuer == c_id),
        "C accepté à nouveau"
    );

    // Re-ban seq 3, puis redémarrage HORS LIGNE de B : le cache protège.
    a.publish_denylist(&list(&editor, 3, &[c_id]))
        .await
        .unwrap();
    poll_until(Duration::from_secs(30), || {
        b.denylist_source(&editor_id).map(|s| s.seq) == Some(3)
    })
    .await;
    drop(b);
    let b2 = node(dir.path(), "b").await; // pas de dial : hors ligne
    assert_eq!(b2.denylist_source(&editor_id).map(|s| s.seq), Some(3));
    let r = b2.apply_feed_for_tests(Feed::build_signed(&kp_c, 4, &[cid]).unwrap());
    assert!(r.is_err() || !b2.catalog_entries().iter().any(|e| e.issuer == c_id));
}

/// Un tiers ne peut pas écraser le record de l'éditeur : un record signé par
/// une autre clé sous la clé DHT de l'éditeur est refusé au filtre entrant.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn third_party_cannot_overwrite_editor_record() {
    let dir = tempfile::tempdir().unwrap();
    let editor = Keypair::generate_ed25519();
    let impostor = Keypair::generate_ed25519();
    let a = node(dir.path(), "a2").await;
    let addr_a = a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    let b = node(dir.path(), "b2").await;
    b.add_address(a.peer_id(), addr_a.clone()).await.unwrap();
    b.dial(addr_a).await.unwrap();
    let editor_id = editor.public().to_peer_id();
    a.publish_denylist(&list(&editor, 1, &[])).await.unwrap();
    b.subscribe_denylist_issuer(editor_id).unwrap();
    poll_until(Duration::from_secs(30), || {
        b.denylist_source(&editor_id).is_some()
    })
    .await;
    // L'imposteur signe une liste seq 99 avec SA clé : `publish_denylist` la met
    // sous SA propre clé DHT, jamais sous celle de l'éditeur → la source de
    // l'éditeur reste à seq 1 chez B.
    a.publish_denylist(&list(&impostor, 99, &[])).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(b.denylist_source(&editor_id).map(|s| s.seq), Some(1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsubscribe_removes_cache_and_entries() {
    let dir = tempfile::tempdir().unwrap();
    let editor = Keypair::generate_ed25519();
    let banned = Keypair::generate_ed25519().public().to_peer_id();
    let b = node(dir.path(), "b3").await;
    b.subscribe_denylist(&list(&editor, 1, &[banned]))
        .await
        .unwrap();
    let editor_id = editor.public().to_peer_id();
    b.subscribe_denylist_issuer(editor_id).unwrap();
    assert!(b.is_key_blocked(&banned));
    b.unsubscribe_denylist_issuer(editor_id).await.unwrap();
    assert!(!b.is_key_blocked(&banned));
    assert!(b.denylist_source(&editor_id).is_none());
}
