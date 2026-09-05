//! Lecture progressive de bout en bout (spec 2026-09-05) : premier segment
//! servi avant la fin, seek prioritaire, politiques Seed/Stream, modération,
//! purge des orphelins.

use champinium_core::content::cid_for;
use champinium_core::identity::load_or_generate;
use champinium_core::ingest::HlsSegment;
use champinium_core::stream::streams_root;
use champinium_core::{Blockstore, Denylist, Feed, HlsManifest, Moderation, Node};
use libp2p::identity::Keypair;
use std::time::Duration;

const UPDATED: &str = "2026-09-05T00:00:00Z";

async fn node(dir: &std::path::Path, name: &str) -> Node {
    node_with(dir, name, Moderation::empty()).await
}

async fn node_with(dir: &std::path::Path, name: &str, m: Moderation) -> Node {
    let kp = load_or_generate(dir.join(format!("{name}.key"))).unwrap();
    let bs = Blockstore::open(dir.join(name)).unwrap();
    Node::with_moderation(kp, bs, m).await.unwrap()
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
    .expect("condition attendue non atteinte sous l'échéance");
}

/// Ouvre une session en réessayant (course d'établissement Kademlia, même
/// motif que `fetch_hls_until_ok`).
async fn open_until_ok(node: &Node, m: cid::Cid) -> champinium_core::StreamSessionInfo {
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

fn seg_url(info: &champinium_core::StreamSessionInfo, n: usize) -> String {
    info.url.replace("index.m3u8", &format!("{n}.ts"))
}

/// Publie chez `creator` un manifeste dont `present` indique, par index, si
/// le segment existe réellement (sinon CID d'un contenu que personne ne
/// détient — jamais récupérable).
async fn publish(creator: &Node, present: &[bool]) -> (cid::Cid, Vec<cid::Cid>) {
    publish_with_duration(creator, present, 1.0).await
}

/// Comme `publish`, avec une durée par segment paramétrable — nécessaire pour
/// placer un segment hors de la fenêtre d'avance passive
/// (`stream::scheduler::LOOKAHEAD_SECS`, 90 s) et ainsi prouver qu'un seek
/// (et non le préchargement d'arrière-plan) l'a ramené.
async fn publish_with_duration(
    creator: &Node,
    present: &[bool],
    duration: f32,
) -> (cid::Cid, Vec<cid::Cid>) {
    let mut cids = vec![];
    for (i, p) in present.iter().enumerate() {
        let payload = format!("segment {i} du test stream");
        let cid = if *p {
            creator.add(payload.as_bytes()).await.unwrap()
        } else {
            cid_for(format!("absent {i}").as_bytes())
        };
        cids.push(cid);
    }
    let manifest = HlsManifest::new(
        duration,
        cids.iter()
            .map(|c| HlsSegment {
                cid: c.to_string(),
                duration,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_segment_is_served_before_the_session_is_complete() {
    let dir = tempfile::tempdir().unwrap();
    let a = node(dir.path(), "a1").await;
    let addr = a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    // Dernier segment introuvable partout : la session ne peut jamais être
    // complète — la lecture du premier n'en dépend pas.
    let (m, _) = publish(&a, &[true, true, false]).await;
    let b = node(dir.path(), "b1").await;
    wire(&b, &a, addr).await;

    let info = open_until_ok(&b, m).await;
    let r = reqwest::get(seg_url(&info, 0)).await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(
        r.bytes().await.unwrap().as_ref(),
        b"segment 0 du test stream"
    );
    let st = b.stream_status(info.id).unwrap();
    assert!(st.fetched_segments < st.total_segments);
    assert_eq!(st.total_segments, 3);
    b.close_stream(info.id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seek_serves_requested_segment_before_intermediates() {
    let dir = tempfile::tempdir().unwrap();
    let a = node(dir.path(), "a2").await;
    let addr = a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    // 12 segments de 10 s (120 s au total) : la fenêtre d'avance passive
    // (`LOOKAHEAD_SECS` = 90 s) ne couvre, depuis la tête 0, que les indices
    // dont la durée cumulée reste ≤ 90 s — donc jusqu'à l'indice 9 au plus.
    // Les indices 1..=10 sont introuvables partout et l'indice 11 est HORS
    // fenêtre : seul un seek direct (`request(11)`, déclenché par la requête
    // HTTP du lecteur) peut le ramener. Si le préchargement d'arrière-plan
    // servait le 11 sans passer par le seek, ce test échouerait faute de
    // pouvoir distinguer les deux chemins ; en le plaçant hors fenêtre, seul
    // le seek peut réussir.
    let mut present = vec![false; 12];
    present[0] = true;
    present[11] = true;
    let (m, _) = publish_with_duration(&a, &present, 10.0).await;
    let b = node(dir.path(), "b2").await;
    wire(&b, &a, addr).await;

    let info = open_until_ok(&b, m).await;
    let r = reqwest::get(seg_url(&info, 11)).await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(
        r.bytes().await.unwrap().as_ref(),
        b"segment 11 du test stream"
    );
    let st = b.stream_status(info.id).unwrap();
    assert!(
        st.fetched_segments <= 2,
        "1..=10 ne doivent pas être présents"
    );
    assert!(
        st.failed_reason.is_none(),
        "NoProviders n'est pas définitif"
    );
    b.close_stream(info.id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribed_channel_seeds_and_unsubscribed_streams() {
    let dir = tempfile::tempdir().unwrap();
    let kp_a = Keypair::generate_ed25519();
    let a = Node::with_moderation(
        kp_a.clone(),
        Blockstore::open(dir.path().join("a3")).unwrap(),
        Moderation::empty(),
    )
    .await
    .unwrap();
    let addr = a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    let (m, cids) = publish(&a, &[true, true]).await;

    // B souscrit → Seed.
    let b = node(dir.path(), "b3").await;
    wire(&b, &a, addr.clone()).await;
    b.subscribe(a.peer_id()).unwrap();
    let feed = Feed::build_signed(&kp_a, 1, &[m]).unwrap();
    b.apply_feed_for_tests(feed).unwrap();
    poll_until(Duration::from_secs(30), || {
        b.catalog_subscribed().iter().any(|e| e.cids.contains(&m))
    })
    .await;
    let info = open_until_ok(&b, m).await;
    for n in 0..2 {
        assert_eq!(reqwest::get(seg_url(&info, n)).await.unwrap().status(), 200);
    }
    poll_until(Duration::from_secs(30), || {
        cids.iter().all(|c| b.blockstore().has(c))
    })
    .await;
    // La complétion réveille le seed proactif → SeedIndex sous quota.
    poll_until(Duration::from_secs(30), || b.storage_stats().0 > 0).await;
    b.close_stream(info.id).await;

    // C ne souscrit pas → Stream : rien au blockstore, dossier purgé.
    let c = node(dir.path(), "c3").await;
    wire(&c, &a, addr).await;
    let info = open_until_ok(&c, m).await;
    for n in 0..2 {
        assert_eq!(reqwest::get(seg_url(&info, n)).await.unwrap().status(), 200);
    }
    assert!(cids.iter().all(|c_| !c.blockstore().has(c_)));
    assert!(!c.blockstore().has(&m));
    let session_dir = streams_root(c.blockstore()).join(info.id.to_string());
    assert!(session_dir.exists());
    c.close_stream(info.id).await;
    assert!(!session_dir.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn moderated_segment_is_403_and_freezes_session() {
    let dir = tempfile::tempdir().unwrap();
    let a = node(dir.path(), "a4").await;
    let addr = a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    // Segment 2 : introuvable partout (jamais présent chez le créateur).
    // Sert à prouver qu'une session figée renvoie 403 immédiatement pour
    // n'importe quel autre segment absent, pas seulement le modéré.
    let (m, cids) = publish(&a, &[true, true, false]).await;

    let issuer = Keypair::generate_ed25519();
    let dl = Denylist::build_signed("test", UPDATED, &issuer, &[cids[1]], &[]).unwrap();
    let mut moderation = Moderation::empty();
    moderation.subscribe(&dl).unwrap();
    let b = node_with(dir.path(), "b4", moderation).await;
    wire(&b, &a, addr).await;

    let info = open_until_ok(&b, m).await;
    assert_eq!(reqwest::get(seg_url(&info, 0)).await.unwrap().status(), 200);
    assert_eq!(reqwest::get(seg_url(&info, 1)).await.unwrap().status(), 403);
    let st = b.stream_status(info.id).unwrap();
    assert!(st.failed_reason.is_some());
    assert!(!b.blockstore().has(&cids[1]));

    // La session est figée : une requête sur un AUTRE segment absent doit
    // rendre 403 sans attendre `STREAM_REQUEST_TIMEOUT` (60 s) — sinon
    // chaque requête immobiliserait une connexion pour rien.
    let started = std::time::Instant::now();
    assert_eq!(reqwest::get(seg_url(&info, 2)).await.unwrap().status(), 403);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "le 403 doit arriver sans attendre le timeout de requête, a pris {:?}",
        started.elapsed()
    );

    b.close_stream(info.id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opening_a_node_purges_orphan_session_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let bs = Blockstore::open(dir.path().join("orph")).unwrap();
    let orphan = streams_root(&bs).join("42");
    std::fs::create_dir_all(&orphan).unwrap();
    std::fs::write(orphan.join("0.ts"), b"x").unwrap();
    let _n = node(dir.path(), "orph").await;
    assert!(!orphan.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_the_node_frees_the_port() {
    let dir = tempfile::tempdir().unwrap();
    let n = node(dir.path(), "drop").await;
    let seg = n.add(b"s").await.unwrap();
    let manifest = HlsManifest::new(
        1.0,
        vec![HlsSegment {
            cid: seg.to_string(),
            duration: 1.0,
        }],
    );
    let m = n.add(manifest.to_json().unwrap().as_bytes()).await.unwrap();
    let info = n.open_stream(m).await.unwrap();
    let port: u16 = info
        .url
        .split(':')
        .nth(2)
        .unwrap()
        .split('/')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    drop(n);
    poll_until(Duration::from_secs(10), || {
        std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
    })
    .await;
}
