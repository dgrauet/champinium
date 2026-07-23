//! `fetch_hls` ne doit pas laisser de sortie partielle (segments `.ts` orphelins
//! sans `index.m3u8`) quand un segment est introuvable ou refusé.
//!
//! Retrait de seed-what-you-consume (spec channels lot c) : `fetch_hls`
//! décide sa politique de stockage en interne — `Seed` si le manifeste
//! appartient à un channel souscrit, `Stream` sinon. Dans les deux cas, le
//! fichier de sortie (`out_dir`) est produit ; seul le sort du blockstore
//! diffère.

use champinium_core::content::cid_for;
use champinium_core::identity::load_or_generate;
use champinium_core::ingest::HlsSegment;
use champinium_core::{Blockstore, Feed, HlsManifest, Moderation, Node};
use libp2p::identity::Keypair;
use std::path::PathBuf;
use std::time::Duration;

/// `fetch_hls` en réessayant jusqu'au succès sous une échéance : après un
/// `dial`, la connexion est initiée mais les provider records du créateur
/// peuvent ne pas être encore visibles côté consommateur (course
/// d'établissement Kademlia/connexion) — un `fetch_hls` immédiat échouerait
/// alors vite en `NoProviders`. On réessaie (déterministe, sans sleep fixe
/// fragile) plutôt que de supposer la disponibilité instantanée (flake CI
/// Windows observé). Réservé aux chemins qui DOIVENT réussir.
async fn fetch_hls_until_ok(
    node: &Node,
    manifest_cid: champinium_core::Cid,
    out: &std::path::Path,
) -> PathBuf {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match node.fetch_hls(manifest_cid, out).await {
                Ok(playlist) => return playlist,
                Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
    })
    .await
    .expect("fetch_hls doit finir par réussir sous l'échéance")
}

/// Comme [`fetch_hls_until_ok`], mais pour le cas souscrit (politique `Seed`
/// attendue) : réessaie jusqu'à ce que le manifeste soit RÉELLEMENT en cache
/// dans le blockstore, pas seulement jusqu'au premier `Ok`. Un `Ok` isolé peut
/// survenir alors que la politique retenue était encore `Stream` (le
/// catalogue souscrit pas encore visible côté consommateur au moment précis
/// du fetch) — dans ce cas rien n'est mis en cache et une seule tentative
/// réussie ne garantit pas l'effet attendu (flake CI observé).
async fn fetch_hls_until_cached(
    node: &Node,
    manifest_cid: champinium_core::Cid,
    out: &std::path::Path,
) -> PathBuf {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(playlist) = node.fetch_hls(manifest_cid, out).await {
                if node.blockstore().has(&manifest_cid) {
                    return playlist;
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect(
        "fetch_hls doit finir par mettre le manifeste en cache (politique Seed) sous l'échéance",
    )
}

/// Poll générique jusqu'à ce que `cond` renvoie `true`, sous une échéance
/// généreuse — même patron que `fetch_hls_until_ok`/`fetch_hls_until_cached`,
/// pour les conditions qui ne passent pas par `fetch_hls`.
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fetch_hls_leaves_no_partial_output_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    let kp = load_or_generate(dir.path().join("node.key")).unwrap();
    let bs = Blockstore::open(dir.path().join("blocks")).unwrap();
    let node = Node::with_moderation(kp, bs, Moderation::empty())
        .await
        .unwrap();

    // Premier segment présent localement (sera écrit), second introuvable :
    // la reconstruction doit échouer APRÈS avoir écrit le premier → c'est le
    // cas qui laissait un `.ts` orphelin sans `index.m3u8`.
    let present_cid = node.add(b"premier segment bien present").await.unwrap();
    let manifest = HlsManifest::new(
        1.0,
        vec![
            HlsSegment {
                cid: present_cid.to_string(),
                duration: 1.0,
            },
            HlsSegment {
                cid: cid_for(b"segment absent").to_string(),
                duration: 1.0,
            },
        ],
    );
    let manifest_cid = node
        .add(manifest.to_json().unwrap().as_bytes())
        .await
        .unwrap();

    let out = dir.path().join("hls-out");
    let res = tokio::time::timeout(Duration::from_secs(30), node.fetch_hls(manifest_cid, &out))
        .await
        .expect("fetch_hls doit se terminer");

    assert!(
        res.is_err(),
        "un segment manquant doit faire échouer fetch_hls"
    );
    assert!(
        !out.join("index.m3u8").exists(),
        "aucun index.m3u8 ne doit être produit sur échec"
    );
    // Aucun segment .ts orphelin ne doit subsister.
    if out.exists() {
        let leftovers: Vec<_> = std::fs::read_dir(&out).unwrap().collect();
        assert!(
            leftovers.is_empty(),
            "aucune sortie partielle ne doit subsister, trouvé: {leftovers:?}"
        );
    }
}

async fn node(dir: &std::path::Path, name: &str) -> Node {
    let kp = load_or_generate(dir.join(format!("{name}.key"))).unwrap();
    let bs = Blockstore::open(dir.join(name)).unwrap();
    Node::with_moderation(kp, bs, Moderation::empty())
        .await
        .unwrap()
}

/// Construit un manifeste HLS à un segment, publié via `creator`, et renvoie
/// (manifest_cid, segment_cid).
async fn publish_one_segment_hls(creator: &Node) -> (cid::Cid, cid::Cid) {
    let seg_cid = creator
        .add(b"segment video du test fetch_hls")
        .await
        .unwrap();
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

/// Channel souscrit : `fetch_hls` doit stocker le manifeste ET les segments
/// dans le blockstore local (politique `Seed` — le consommateur suit
/// activement ce channel, il doit pouvoir le resservir).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fetch_hls_stores_content_for_subscribed_channel() {
    let dir = tempfile::tempdir().unwrap();

    let kp_creator = Keypair::generate_ed25519();
    let bs_creator = Blockstore::open(dir.path().join("creator")).unwrap();
    let node_creator = Node::with_moderation(kp_creator.clone(), bs_creator, Moderation::empty())
        .await
        .unwrap();
    let addr_creator = node_creator
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();

    let (manifest_cid, seg_cid) = publish_one_segment_hls(&node_creator).await;

    let node_consumer = node(dir.path(), "consumer-sub").await;
    node_consumer
        .add_address(node_creator.peer_id(), addr_creator.clone())
        .await
        .unwrap();
    node_consumer.dial(addr_creator).await.unwrap();

    // B se souscrit au créateur et voit son feed (injecté directement pour un
    // test déterministe — le suivi réseau réel est couvert par subscriptions.rs).
    node_consumer.subscribe(node_creator.peer_id()).unwrap();
    let feed = Feed::build_signed(&kp_creator, 1, &[manifest_cid]).unwrap();
    node_consumer.apply_feed_for_tests(feed).unwrap();
    // Poll (pas une seule lecture) : le catalogue souscrit doit vraiment
    // contenir le manifeste AVANT le fetch, sinon `fetch_hls` retiendrait la
    // politique `Stream` (course catalogue vs fetch, flake CI observé).
    poll_until(Duration::from_secs(30), || {
        node_consumer
            .catalog_subscribed()
            .iter()
            .any(|e| e.cids.contains(&manifest_cid))
    })
    .await;

    let out = dir.path().join("hls-out-sub");
    let playlist = fetch_hls_until_cached(&node_consumer, manifest_cid, &out).await;
    assert!(playlist.exists());

    assert!(
        node_consumer.blockstore().has(&manifest_cid),
        "channel souscrit : le manifeste doit être en cache"
    );
    assert!(
        node_consumer.blockstore().has(&seg_cid),
        "channel souscrit : le segment doit être en cache"
    );

    // M1 (revue finale lot c) : `get_with(Seed)` mettait déjà en cache, mais
    // sans jamais entrer au SeedIndex — le quota ne bougeait pas et un
    // désabonnement ultérieur ne purgeait rien. Les deux doivent maintenant
    // suivre le fetch.
    let (used, _quota) = node_consumer.storage_stats();
    assert!(
        used > 0,
        "storage_stats().used doit croître après un fetch_hls seedé"
    );

    node_consumer.unsubscribe(node_creator.peer_id()).unwrap();
    // Poll (pas un assert immédiat) : un traînard du seed_loop ou un retard de
    // comptabilité du SeedIndex peut laisser ces conditions momentanément
    // fausses juste après `unsubscribe` (même classe de flake que
    // `key_moderation`, cf. commentaire du module).
    poll_until(Duration::from_secs(30), || {
        !node_consumer.blockstore().has(&manifest_cid)
            && !node_consumer.blockstore().has(&seg_cid)
            && node_consumer.storage_stats().0 == 0
    })
    .await;
}

/// Channel NON souscrit (ex. consultation depuis Explorer) : `fetch_hls`
/// produit toujours la sortie jouable dans `out_dir`, mais rien n'entre dans
/// le blockstore local (politique `Stream`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fetch_hls_streams_content_for_unsubscribed_channel() {
    let dir = tempfile::tempdir().unwrap();

    let node_creator = node(dir.path(), "creator-unsub").await;
    let addr_creator = node_creator
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();

    let (manifest_cid, seg_cid) = publish_one_segment_hls(&node_creator).await;

    let node_consumer = node(dir.path(), "consumer-unsub").await;
    node_consumer
        .add_address(node_creator.peer_id(), addr_creator.clone())
        .await
        .unwrap();
    node_consumer.dial(addr_creator).await.unwrap();

    // Aucun abonnement : `catalog_subscribed()` est vide, la politique
    // retenue par `fetch_hls` est donc `Stream`.
    assert!(node_consumer.catalog_subscribed().is_empty());

    let out = dir.path().join("hls-out-unsub");
    let playlist = fetch_hls_until_ok(&node_consumer, manifest_cid, &out).await;
    assert!(playlist.exists(), "index.m3u8 doit exister dans out_dir");

    assert!(
        !node_consumer.blockstore().has(&manifest_cid),
        "channel non souscrit : le manifeste ne doit pas être mis en cache"
    );
    assert!(
        !node_consumer.blockstore().has(&seg_cid),
        "channel non souscrit : le segment ne doit pas être mis en cache"
    );
}
