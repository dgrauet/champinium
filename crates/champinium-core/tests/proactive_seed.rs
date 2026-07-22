//! Seed proactif des channels souscrits (spec channels lot c) : quota,
//! éviction par réplication, pins, purge au désabonnement.
//!
//! Patron des tests deux/trois nœuds : voir `tests/subscriptions.rs`. Le feed
//! d'un émetteur est injecté côté abonné via `apply_feed_for_tests` (comme
//! `tests/fetch_hls.rs::fetch_hls_stores_content_for_subscribed_channel`) —
//! déterministe, ne dépend pas du gossip réel (couvert ailleurs). Les
//! intervalles de boucle sont courts (constructeur, jamais un setter — voir
//! `Node::with_moderation_and_intervals`) ; les délais de convergence sont
//! généreux (30 s) pour éviter le flake observé en CI sous charge (cf.
//! `subscriptions.rs`).

use champinium_core::identity::load_or_generate;
use champinium_core::ingest::HlsSegment;
use champinium_core::{seeding, Blockstore, Feed, HlsManifest, Moderation, Node};
use libp2p::identity::Keypair;
use std::path::Path;
use std::time::Duration;

/// Intervalle court pour toutes les boucles de fond (suivi ET seed) — les
/// tests ne veulent pas attendre 5 min.
const FAST: Duration = Duration::from_millis(100);
/// Délai de convergence généreux (leçon anti-flake CI, cf. `subscriptions.rs`).
const CONVERGE: Duration = Duration::from_secs(30);

async fn node(dir: &Path, name: &str) -> Node {
    let kp = load_or_generate(dir.join(format!("{name}.key"))).unwrap();
    let bs = Blockstore::open(dir.join(name)).unwrap();
    Node::with_moderation_and_intervals(kp, bs, Moderation::empty(), FAST, FAST)
        .await
        .unwrap()
}

/// Comme [`node`], mais avec un quota de seed minuscule persisté AVANT la
/// construction (le quota est chargé au constructeur, comme l'index et les
/// abonnements — voir `seeding::load_seed_quota`).
async fn node_with_quota(dir: &Path, name: &str, quota_bytes: u64) -> Node {
    let kp = load_or_generate(dir.join(format!("{name}.key"))).unwrap();
    let bs = Blockstore::open(dir.join(name)).unwrap();
    seeding::save_seed_quota(&bs, quota_bytes).unwrap();
    Node::with_moderation_and_intervals(kp, bs, Moderation::empty(), FAST, FAST)
        .await
        .unwrap()
}

/// Publie (localement, `add`) un manifeste HLS à un segment depuis `creator`
/// et renvoie `(manifest_cid, seg_cid)`.
async fn publish_one_segment(creator: &Node, payload: &[u8]) -> (cid::Cid, cid::Cid) {
    let seg_cid = creator.add(payload).await.unwrap();
    let manifest = HlsManifest::new(
        1.0,
        vec![HlsSegment {
            cid: seg_cid.to_string(),
            duration: 1.0,
        }],
    );
    let manifest_cid = creator
        .add(manifest.to_json().unwrap().as_bytes())
        .await
        .unwrap();
    (manifest_cid, seg_cid)
}

/// Injecte, côté `subscriber`, le feed signé de `issuer_kp` listant `cids` —
/// simule la réception d'un feed sans dépendre du gossip réel (déjà couvert
/// par `feed_gossip.rs`/`subscriptions.rs`).
fn inject_feed(subscriber: &Node, issuer_kp: &Keypair, seq: u64, cids: &[cid::Cid]) {
    let feed = Feed::build_signed(issuer_kp, seq, cids).unwrap();
    subscriber.apply_feed_for_tests(feed).unwrap();
}

async fn connect(a: &Node, b: &Node) {
    let addr = a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    b.add_address(a.peer_id(), addr.clone()).await.unwrap();
    b.dial(addr).await.unwrap();
}

/// (a) B s'abonne à A → les segments de la publication de A arrivent dans le
/// blockstore de B, et B devient fournisseur — SANS AUCUNE lecture explicite
/// (`get`/`fetch_hls`) : c'est `seed_loop` seul qui doit faire le travail.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribing_proactively_seeds_publication_without_any_read() {
    let dir = tempfile::tempdir().unwrap();

    let kp_a = Keypair::generate_ed25519();
    let bs_a = Blockstore::open(dir.path().join("a")).unwrap();
    let node_a = Node::with_moderation(kp_a.clone(), bs_a, Moderation::empty())
        .await
        .unwrap();
    let (manifest_cid, seg_cid) = publish_one_segment(&node_a, b"segment de A, seede par B").await;

    let node_b = node(dir.path(), "b").await;
    connect(&node_a, &node_b).await;
    node_b.subscribe(node_a.peer_id()).unwrap();
    inject_feed(&node_b, &kp_a, 1, &[manifest_cid]);

    tokio::time::timeout(CONVERGE, async {
        loop {
            if node_b.blockstore().has(&manifest_cid) && node_b.blockstore().has(&seg_cid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    })
    .await
    .expect("le seed proactif doit récupérer manifeste + segment sans lecture explicite");

    // B doit s'être annoncé fournisseur (Seed, pas Stream).
    let providers = node_b.get_providers(seg_cid).await.unwrap();
    assert!(
        providers.contains(&node_b.peer_id()),
        "B doit devenir fournisseur du segment après l'avoir seedé"
    );

    let (seeded, total) = node_b.seed_status(node_a.peer_id());
    assert_eq!((seeded, total), (1, 1));
}

/// (b) — critère de démo spec : A publie, B (abonné) le seede proactivement,
/// A s'éteint, puis C obtient le contenu identique depuis B SEUL.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_publisher_content_survives_via_proactive_seeder() {
    let dir = tempfile::tempdir().unwrap();

    let kp_a = Keypair::generate_ed25519();
    let bs_a = Blockstore::open(dir.path().join("a")).unwrap();
    let node_a = Node::with_moderation(kp_a.clone(), bs_a, Moderation::empty())
        .await
        .unwrap();
    let payload = b"contenu de A, doit survivre a son extinction".to_vec();
    let (manifest_cid, seg_cid) = publish_one_segment(&node_a, &payload).await;

    let node_b = node(dir.path(), "b").await;
    connect(&node_a, &node_b).await;
    node_b.subscribe(node_a.peer_id()).unwrap();
    inject_feed(&node_b, &kp_a, 1, &[manifest_cid]);

    tokio::time::timeout(CONVERGE, async {
        loop {
            if node_b.blockstore().has(&manifest_cid) && node_b.blockstore().has(&seg_cid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    })
    .await
    .expect("B doit avoir proactivement seedé avant l'extinction de A");

    // A s'éteint : plus aucune poignée `Node` ne le référence → sa boucle
    // d'évènements et ses tâches de fond s'arrêtent (cf. `alive`).
    drop(node_a);

    // C rejoint le réseau via B UNIQUEMENT (jamais connecté à A).
    let node_c = node(dir.path(), "c").await;
    connect(&node_b, &node_c).await;

    let fetched = tokio::time::timeout(CONVERGE, async {
        loop {
            if let Ok(bytes) = node_c.get(seg_cid).await {
                return bytes;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("C doit récupérer le contenu depuis B seul, A étant hors ligne");
    assert_eq!(fetched, payload);
}

/// (c) Quota minuscule (juste assez pour le manifeste, pas pour manifeste +
/// segment) → la publication N'EST PAS seedée : ni indexée, ni laissée en
/// orphelin dans le magasin (pas de dépassement, pas de fuite de comptabilité
/// — cf. rapport tâche c4 §Minor, `SeedIndex` est la source de vérité).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tiny_quota_skips_publication_without_overshoot() {
    let dir = tempfile::tempdir().unwrap();

    let kp_a = Keypair::generate_ed25519();
    let bs_a = Blockstore::open(dir.path().join("a")).unwrap();
    let node_a = Node::with_moderation(kp_a.clone(), bs_a, Moderation::empty())
        .await
        .unwrap();
    let (manifest_cid, seg_cid) =
        publish_one_segment(&node_a, b"segment qui ne doit jamais arriver sous quota").await;
    let manifest_size = node_a.blockstore().size_of(&manifest_cid).unwrap();

    // Quota = exactement la taille du manifeste : il rentre seul, mais plus
    // rien pour le segment, et rien d'autre n'est indexé pour évincer.
    let node_b = node_with_quota(dir.path(), "b", manifest_size).await;
    connect(&node_a, &node_b).await;
    node_b.subscribe(node_a.peer_id()).unwrap();
    inject_feed(&node_b, &kp_a, 1, &[manifest_cid]);

    // Laisse plusieurs passes de seed_loop s'exécuter (FAST = 100 ms).
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert!(
        !node_b.blockstore().has(&seg_cid),
        "le segment ne doit jamais être récupéré sous un quota trop petit"
    );
    assert!(
        !node_b.blockstore().has(&manifest_cid),
        "le manifeste orphelin doit être retiré (rollback) plutôt que de fuiter hors comptabilité"
    );
    let (seeded, total) = node_b.seed_status(node_a.peer_id());
    assert_eq!((seeded, total), (0, 1));
    let (used, quota) = node_b.storage_stats();
    assert_eq!(quota, manifest_size);
    assert!(used <= quota, "le quota ne doit jamais être dépassé");
}

/// (d) Désabonnement : blocs purgés, SAUF la publication épinglée.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsubscribe_purges_blocks_except_pinned() {
    let dir = tempfile::tempdir().unwrap();

    let kp_a = Keypair::generate_ed25519();
    let bs_a = Blockstore::open(dir.path().join("a")).unwrap();
    let node_a = Node::with_moderation(kp_a.clone(), bs_a, Moderation::empty())
        .await
        .unwrap();
    let (pinned_manifest, pinned_seg) = publish_one_segment(&node_a, b"publication epinglee").await;
    let (other_manifest, other_seg) =
        publish_one_segment(&node_a, b"publication non epinglee").await;

    let node_b = node(dir.path(), "b").await;
    connect(&node_a, &node_b).await;
    node_b.subscribe(node_a.peer_id()).unwrap();
    inject_feed(&node_b, &kp_a, 1, &[pinned_manifest, other_manifest]);

    tokio::time::timeout(CONVERGE, async {
        loop {
            let (seeded, total) = node_b.seed_status(node_a.peer_id());
            if seeded == total && total == 2 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    })
    .await
    .expect("les deux publications doivent être seedées avant le désabonnement");

    node_b.pin(pinned_manifest).unwrap();
    node_b.unsubscribe(node_a.peer_id()).unwrap();

    assert!(
        node_b.blockstore().has(&pinned_manifest) && node_b.blockstore().has(&pinned_seg),
        "la publication épinglée doit survivre au désabonnement"
    );
    assert!(
        !node_b.blockstore().has(&other_manifest) && !node_b.blockstore().has(&other_seg),
        "la publication non épinglée doit être purgée au désabonnement"
    );
}

/// (e) Contenu propre ingéré = épinglé d'office : même sous un quota qui
/// forcerait par ailleurs son éviction (rien d'autre n'est évictable pour
/// faire de la place à une publication tierce souscrite), il n'est JAMAIS
/// évincé.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn own_ingested_content_is_pinned_and_never_evicted() {
    if !ffmpeg_available().await {
        eprintln!("ffmpeg absent — test d'auto-pin à l'ingestion ignoré");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mp4");
    assert!(
        generate_media(&input, 3).await,
        "génération du média de test"
    );

    let kp_b = load_or_generate(dir.path().join("b.key")).unwrap();
    let bs_b = Blockstore::open(dir.path().join("b")).unwrap();
    let node_b = Node::with_moderation(kp_b, bs_b, Moderation::empty())
        .await
        .unwrap();
    let own_manifest = node_b.ingest_file(&input).await.unwrap();
    assert!(node_b.blockstore().has(&own_manifest));
    let own_bytes = node_b.blockstore().size_of(&own_manifest).unwrap();
    let (used_after_ingest, _) = node_b.storage_stats();
    assert!(used_after_ingest >= own_bytes);

    // Quota juste assez pour le contenu propre déjà indexé : aucune place
    // pour une publication tierce, et rien à évincer (le seul contenu
    // indexé est épinglé).
    node_b.set_seed_quota(used_after_ingest).unwrap();

    let kp_a = Keypair::generate_ed25519();
    let bs_a = Blockstore::open(dir.path().join("a")).unwrap();
    let node_a = Node::with_moderation(kp_a.clone(), bs_a, Moderation::empty())
        .await
        .unwrap();
    let (third_manifest, third_seg) =
        publish_one_segment(&node_a, b"publication tierce qui ne doit jamais evincer B").await;
    connect(&node_a, &node_b).await;
    node_b.subscribe(node_a.peer_id()).unwrap();
    inject_feed(&node_b, &kp_a, 1, &[third_manifest]);

    tokio::time::sleep(Duration::from_secs(2)).await;

    assert!(
        node_b.blockstore().has(&own_manifest),
        "le contenu propre épinglé ne doit jamais être évincé"
    );
    assert!(
        !node_b.blockstore().has(&third_seg),
        "la publication tierce ne doit pas être seedée faute de place évictable"
    );
}

async fn ffmpeg_available() -> bool {
    tokio::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

async fn generate_media(out: &Path, secs: u32) -> bool {
    tokio::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i"])
        .arg(format!("testsrc=duration={secs}:size=160x90:rate=15"))
        .args(["-f", "lavfi", "-i"])
        .arg(format!("sine=frequency=440:duration={secs}"))
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-c:a",
            "aac",
            "-t",
        ])
        .arg(secs.to_string())
        .arg(out)
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}
