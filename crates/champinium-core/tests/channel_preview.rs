//! Aperçu de channel par lien (spec 2026-07-23, partie A) : résolution
//! catalogue-d'abord puis DHT, états subscribed/blocked reflétés.

use champinium_core::catalog::CatalogItem;
use champinium_core::content::cid_for;
use champinium_core::feed::{ChannelMeta, Feed, FeedEntry};
use champinium_core::identity::load_or_generate;
use champinium_core::{Blockstore, CoreError, Moderation, Node};
use libp2p::identity::Keypair;
use std::time::Duration;

async fn node(dir: &std::path::Path, name: &str) -> Node {
    let kp = load_or_generate(dir.join(format!("{name}.key"))).unwrap();
    let bs = Blockstore::open(dir.join(name)).unwrap();
    Node::with_moderation(kp, bs, Moderation::empty())
        .await
        .unwrap()
}

async fn connect(a: &Node, b: &Node) {
    let addr = a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    b.add_address(a.peer_id(), addr.clone()).await.unwrap();
    b.dial(addr).await.unwrap();
}

/// Réessaie `resolve_channel` jusqu'à succès (course connue dial→providers,
/// même patron que `fetch_hls.rs::fetch_hls_until_ok`).
async fn resolve_until_ok(
    node: &Node,
    issuer: libp2p::PeerId,
) -> champinium_core::p2p::ChannelPreview {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match node.resolve_channel(issuer).await {
                Ok(preview) => return preview,
                Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
    })
    .await
    .expect("resolve_channel doit finir par réussir sous l'échéance")
}

/// (a) Channel déjà au catalogue : résolution IMMÉDIATE (aucun réseau) —
/// nœud isolé, feed injecté via `apply_feed_for_tests`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolves_immediately_from_catalog_without_network() {
    let dir = tempfile::tempdir().unwrap();
    let node = node(dir.path(), "solo").await;

    let other = Keypair::generate_ed25519();
    let issuer = other.public().to_peer_id();
    let cid = cid_for(b"contenu du catalogue");
    let channel = ChannelMeta {
        name: "Aperçu".into(),
        description: "desc".into(),
        avatar_cid: None,
    };
    let entries = vec![FeedEntry {
        cid: cid.to_string(),
        title: "Titre".into(),
        tags: vec!["tag".into()],
    }];
    let feed = Feed::build_signed_with(&other, 1, &channel, &entries).unwrap();
    node.apply_feed_for_tests(feed).unwrap();

    let preview = node.resolve_channel(issuer).await.unwrap();
    assert_eq!(preview.issuer, issuer);
    assert_eq!(preview.channel.name, "Aperçu");
    assert_eq!(preview.items.len(), 1);
    assert_eq!(preview.items[0].title, "Titre");
    assert!(!preview.subscribed);
    assert!(!preview.blocked);
}

/// (b) Channel inconnu localement : A publie (DHT PUT), B (connecté, non
/// abonné) `resolve_channel(A)` → `fetch_feed` → aperçu peuplé.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolves_unknown_channel_via_dht() {
    let dir = tempfile::tempdir().unwrap();
    let node_a = node(dir.path(), "a").await;
    let node_b = node(dir.path(), "b").await;
    connect(&node_a, &node_b).await;

    node_a
        .set_channel_profile(ChannelMeta {
            name: "Créateur".into(),
            description: "".into(),
            avatar_cid: None,
        })
        .await
        .unwrap();
    let cid = cid_for(b"contenu dht");
    node_a.publish_feed(&[cid]).await.unwrap();

    let preview = resolve_until_ok(&node_b, node_a.peer_id()).await;
    assert_eq!(preview.issuer, node_a.peer_id());
    assert_eq!(preview.channel.name, "Créateur");
    assert!(preview.items.iter().any(|i: &CatalogItem| i.cid == cid));
    assert!(!preview.subscribed);
    assert!(!preview.blocked);
}

/// (c) subscribed=true après `node.subscribe(issuer)`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribed_reflects_subscription_state() {
    let dir = tempfile::tempdir().unwrap();
    let node_a = node(dir.path(), "a").await;
    let node_b = node(dir.path(), "b").await;
    connect(&node_a, &node_b).await;

    let cid = cid_for(b"contenu souscrit");
    node_a.publish_feed(&[cid]).await.unwrap();

    node_b.subscribe(node_a.peer_id()).unwrap();
    let preview = resolve_until_ok(&node_b, node_a.peer_id()).await;
    assert!(preview.subscribed);
    assert!(!preview.blocked);
}

/// (d) blocked=true après `node.block_channel(issuer)` — l'aperçu reste
/// résolvable même si le blocage a purgé l'entrée de catalogue (lot d).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocked_channel_remains_resolvable() {
    let dir = tempfile::tempdir().unwrap();
    let node_a = node(dir.path(), "a").await;
    let node_b = node(dir.path(), "b").await;
    connect(&node_a, &node_b).await;

    let cid = cid_for(b"contenu bloque");
    node_a.publish_feed(&[cid]).await.unwrap();

    // B se souscrit d'abord pour garantir que le feed de A est bien connu
    // (peu importe : le blocage purge le catalogue de toute façon).
    node_b.subscribe(node_a.peer_id()).unwrap();
    let _ = resolve_until_ok(&node_b, node_a.peer_id()).await;

    node_b.block_channel(node_a.peer_id()).await.unwrap();
    assert!(
        !node_b
            .catalog_entries()
            .iter()
            .any(|e| e.issuer == node_a.peer_id()),
        "le blocage doit purger l'entrée de catalogue (lot d)"
    );

    let preview = resolve_until_ok(&node_b, node_a.peer_id()).await;
    assert_eq!(preview.issuer, node_a.peer_id());
    assert!(preview.blocked);
    assert!(
        !preview.subscribed,
        "le blocage désabonne (lot d) — jamais souscrit pour un bloqué"
    );
    assert!(
        !node_b
            .catalog_entries()
            .iter()
            .any(|e| e.issuer == node_a.peer_id()),
        "resolve_channel ne doit PAS réinsérer une clé bloquée au catalogue"
    );
}

/// Régression (revue finale, panic FFI atteignable) : `Catalog::apply`
/// refuse un émetteur inconnu ET non souscrit dès que le catalogue est à sa
/// borne anti-DoS (`DEFAULT_MAX_ISSUERS`, 1024) — `fetch_feed_inner` ignore
/// ce refus silencieusement. `resolve_channel` ne doit donc PAS supposer que
/// la relecture du catalogue après `fetch_feed` retrouve toujours l'entrée
/// (un `expect` sur ce chemin paniquait, atteignable depuis la FFI par un
/// simple lien collé sur un catalogue plein) : il doit construire l'aperçu
/// depuis le `Feed` déjà vérifié, sans jamais insérer ni paniquer, et laisser
/// le catalogue inchangé.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolves_unknown_issuer_via_dht_even_when_catalog_is_at_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let node_a = node(dir.path(), "a").await;
    let node_b = node(dir.path(), "b").await;
    connect(&node_a, &node_b).await;

    // Sature le catalogue de B à sa borne, avec des émetteurs sans rapport
    // avec A ni entre eux — aucun souscrit, donc aucune exemption de borne.
    for i in 0..champinium_core::catalog::DEFAULT_MAX_ISSUERS {
        let filler = Keypair::generate_ed25519();
        let cid = cid_for(format!("contenu de remplissage {i}").as_bytes());
        let feed = Feed::build_signed(&filler, 1, &[cid]).unwrap();
        node_b.apply_feed_for_tests(feed).unwrap();
    }
    assert_eq!(
        node_b.catalog_entries().len(),
        champinium_core::catalog::DEFAULT_MAX_ISSUERS,
        "catalogue saturé à sa borne avant le test"
    );

    node_a
        .set_channel_profile(ChannelMeta {
            name: "Surnuméraire".into(),
            description: "".into(),
            avatar_cid: None,
        })
        .await
        .unwrap();
    let cid = cid_for(b"contenu d'un emetteur inconnu sur catalogue plein");
    node_a.publish_feed(&[cid]).await.unwrap();

    let preview = resolve_until_ok(&node_b, node_a.peer_id()).await;
    assert_eq!(preview.issuer, node_a.peer_id());
    assert_eq!(preview.channel.name, "Surnuméraire");
    assert!(preview.items.iter().any(|i: &CatalogItem| i.cid == cid));
    assert!(!preview.subscribed);
    assert!(!preview.blocked);

    assert_eq!(
        node_b.catalog_entries().len(),
        champinium_core::catalog::DEFAULT_MAX_ISSUERS,
        "un émetteur inconnu résolu sur catalogue plein ne doit PAS y être inséré"
    );
    assert!(
        !node_b
            .catalog_entries()
            .iter()
            .any(|e| e.issuer == node_a.peer_id()),
        "toujours refusé par la borne anti-DoS (non souscrit)"
    );
}

/// (e) Inconnu et injoignable (nœud isolé, issuer aléatoire) →
/// `Err(NoProviders)`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_unreachable_issuer_returns_no_providers() {
    let dir = tempfile::tempdir().unwrap();
    let node = node(dir.path(), "solo").await;
    let random_issuer = Keypair::generate_ed25519().public().to_peer_id();

    let err = node.resolve_channel(random_issuer).await.unwrap_err();
    assert!(matches!(err, CoreError::NoProviders(_)));
}
