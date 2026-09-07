//! Test d'intégration : « seed de ce que je regarde » (spec persistance §3-4).
//!
//! Opt-in (`set_seed_watched`), hors abonnement, sous le MÊME quota : une
//! lecture complète d'un manifeste attribué à un émetteur du catalogue entre
//! au `SeedIndex` et fait de B un fournisseur de plus. Sans le réglage, rien
//! ne reste. Fermée avant la fin, la session ne laisse aucun orphelin.

use champinium_core::identity::load_or_generate;
use champinium_core::ingest::HlsSegment;
use champinium_core::{Blockstore, Feed, HlsManifest, Moderation, Node};
use libp2p::identity::Keypair;
use std::time::Duration;

async fn node(dir: &std::path::Path, name: &str) -> Node {
    let kp = load_or_generate(dir.join(format!("{name}.key"))).unwrap();
    let bs = Blockstore::open(dir.join(name)).unwrap();
    Node::with_moderation(kp, bs, Moderation::empty())
        .await
        .unwrap()
}

async fn poll_until<F: Fn() -> bool>(deadline: Duration, message: &str, cond: F) {
    tokio::time::timeout(deadline, async {
        loop {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{message}"));
}

/// Ouvre une session en réessayant (course d'établissement Kademlia, même
/// motif que `open_until_ok` de `tests/stream.rs`).
async fn open_until_ok(node: &Node, m: champinium_core::Cid) -> champinium_core::StreamSessionInfo {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match node.open_stream(m).await {
                Ok(i) => return i,
                Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
    })
    .await
    .expect("open_stream doit finir par réussir")
}

/// Publie chez `creator` un manifeste de `n` segments réels et rend
/// `(cid du manifeste, cids des segments)`.
async fn publish(
    creator: &Node,
    tag: &str,
    n: usize,
) -> (champinium_core::Cid, Vec<champinium_core::Cid>) {
    let mut cids = vec![];
    for i in 0..n {
        let payload = format!("segment {i} de {tag} (test seed regarde)");
        cids.push(creator.add(payload.as_bytes()).await.unwrap());
    }
    let manifest = HlsManifest::new(
        1.0,
        cids.iter()
            .map(|c| HlsSegment {
                cid: c.to_string(),
                duration: 1.0,
            })
            .collect(),
    );
    let m = creator
        .add(manifest.to_json().unwrap().as_bytes())
        .await
        .unwrap();
    (m, cids)
}

async fn wire(consumer: &Node, creator: &Node, addr: libp2p::Multiaddr) {
    consumer
        .add_address(creator.peer_id(), addr.clone())
        .await
        .unwrap();
    consumer.dial(addr).await.unwrap();
}

/// Fait entrer le feed signé de `creator` au catalogue de `consumer` **sans
/// abonnement** — c'est ce qui donne un émetteur au manifeste, condition du
/// seed de ce qui est regardé.
async fn seed_catalog(consumer: &Node, creator_key: &Keypair, m: champinium_core::Cid) {
    let feed = Feed::build_signed(creator_key, 1, &[m]).unwrap();
    consumer.apply_feed_for_tests(feed).unwrap();
    poll_until(
        Duration::from_secs(30),
        "le catalogue du consommateur doit voir le manifeste",
        || {
            consumer
                .catalog_entries()
                .iter()
                .any(|e| e.cids.contains(&m))
        },
    )
    .await;
}

/// Réglage actif : une lecture complète hors abonnement est retenue,
/// comptabilisée au quota, et fait de B un fournisseur de plus (réplication
/// 1 → 2, mesurée par un tiers).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn watched_stream_enters_seed_index_when_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let kp_a = Keypair::generate_ed25519();
    let a = Node::with_moderation(
        kp_a.clone(),
        Blockstore::open(dir.path().join("a1")).unwrap(),
        Moderation::empty(),
    )
    .await
    .unwrap();
    let addr_a = a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    let (m, cids) = publish(&a, "on", 2).await;

    // B : PAS abonné, mais « conserver ce que je regarde » actif.
    let b = node(dir.path(), "b1").await;
    b.listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    wire(&b, &a, addr_a.clone()).await;
    b.set_seed_watched(true).unwrap();
    seed_catalog(&b, &kp_a, m).await;
    assert!(
        b.subscriptions().is_empty(),
        "le scénario doit rester hors abonnement"
    );

    let info = open_until_ok(&b, m).await;
    poll_until(
        Duration::from_secs(60),
        "la session doit récupérer tous ses segments",
        || {
            b.stream_status(info.id)
                .map(|s| s.fetched_segments == s.total_segments)
                .unwrap_or(false)
        },
    )
    .await;

    // Complétion → demande d'indexation immédiate → SeedIndex sous quota.
    poll_until(
        Duration::from_secs(10),
        "la publication regardée doit être comptabilisée au quota",
        || b.storage_stats().0 > 0,
    )
    .await;
    poll_until(
        Duration::from_secs(10),
        "l'entrée de catalogue de A doit compter 1 publication retenue",
        || {
            b.catalog_entries()
                .iter()
                .find(|e| e.issuer == a.peer_id())
                .map(|e| b.seed_coverage(&e.cids).0 == 1)
                .unwrap_or(false)
        },
    )
    .await;
    assert!(b.blockstore().has(&m), "le manifeste est retenu");
    assert!(
        cids.iter().all(|c| b.blockstore().has(c)),
        "les segments sont retenus"
    );
    b.close_stream(info.id).await;

    // Un tiers voit deux fournisseurs pour la racine (A + B).
    let c = node(dir.path(), "c1").await;
    c.listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    wire(&c, &a, addr_a).await;
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if c.replication_factor(m).await.unwrap_or(0) >= 2 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    })
    .await
    .expect("la réplication du manifeste doit passer de 1 à 2");
}

/// Réglage inactif (le défaut) : la même lecture ne laisse rien — ni
/// manifeste, ni segment, ni octet au quota.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn watched_stream_ignored_when_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let kp_a = Keypair::generate_ed25519();
    let a = Node::with_moderation(
        kp_a.clone(),
        Blockstore::open(dir.path().join("a2")).unwrap(),
        Moderation::empty(),
    )
    .await
    .unwrap();
    let addr_a = a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    let (m, cids) = publish(&a, "off", 2).await;

    let b = node(dir.path(), "b2").await;
    wire(&b, &a, addr_a).await;
    assert!(!b.seed_watched(), "défaut inactif");
    seed_catalog(&b, &kp_a, m).await;

    let info = open_until_ok(&b, m).await;
    poll_until(
        Duration::from_secs(60),
        "la session doit récupérer tous ses segments",
        || {
            b.stream_status(info.id)
                .map(|s| s.fetched_segments == s.total_segments)
                .unwrap_or(false)
        },
    )
    .await;
    // Laisse le temps à une (mauvaise) indexation d'arriver.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(b.storage_stats().0, 0, "rien ne doit être comptabilisé");
    assert!(!b.blockstore().has(&m), "le manifeste ne doit pas rester");
    assert!(
        cids.iter().all(|c| !b.blockstore().has(c)),
        "aucun segment ne doit rester au magasin de B"
    );
    b.close_stream(info.id).await;
}

/// Session regardée fermée AVANT sa complétion : elle n'entre jamais à
/// l'index, donc ses blocs déjà récupérés doivent être purgés — pas
/// d'accumulation hors comptabilité de quota (spec persistance §4).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn incomplete_watched_stream_leaves_no_orphans() {
    let dir = tempfile::tempdir().unwrap();
    let kp_a = Keypair::generate_ed25519();
    let a = Node::with_moderation(
        kp_a.clone(),
        Blockstore::open(dir.path().join("a3")).unwrap(),
        Moderation::empty(),
    )
    .await
    .unwrap();
    let addr_a = a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    // Assez de segments pour pouvoir fermer entre le premier et le dernier.
    let (m, cids) = publish(&a, "partiel", 12).await;

    let b = node(dir.path(), "b3").await;
    wire(&b, &a, addr_a).await;
    b.set_seed_watched(true).unwrap();
    seed_catalog(&b, &kp_a, m).await;

    let info = open_until_ok(&b, m).await;
    // Attend au moins un segment récupéré, mais ferme avant la complétion :
    // si la session finit d'elle-même avant qu'on la ferme, le test n'a plus
    // de sens — on l'annonce plutôt que de conclure à tort.
    let mut closed_incomplete = false;
    for _ in 0..600 {
        let st = b.stream_status(info.id).unwrap();
        if st.fetched_segments >= 1 && st.fetched_segments < st.total_segments {
            b.close_stream(info.id).await;
            closed_incomplete = true;
            break;
        }
        if st.fetched_segments == st.total_segments {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        closed_incomplete,
        "la session s'est complétée avant d'avoir pu être fermée en cours de route"
    );

    assert_eq!(
        b.storage_stats().0,
        0,
        "une session incomplète ne compte rien au quota"
    );
    assert!(
        !b.blockstore().has(&m),
        "le manifeste d'une session incomplète ne doit pas rester"
    );
    assert!(
        cids.iter().all(|c| !b.blockstore().has(c)),
        "aucun segment orphelin ne doit rester au magasin de B"
    );
}
