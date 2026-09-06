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

/// Interroge `node.get_providers(cid)` jusqu'à ce que `target` y figure, sous
/// une échéance généreuse — même patron que `fetch`/`reprovide_makes_stored_
/// blocks_discoverable` (`tests/seeding.rs`) : une requête DHT immédiate peut
/// précéder la convergence Kademlia après un `dial` récent.
async fn wait_for_provider(
    node: &Node,
    cid: champinium_core::Cid,
    target: champinium_core::PeerId,
) -> std::collections::HashSet<champinium_core::PeerId> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(providers) = node.get_providers(cid).await {
                if providers.contains(&target) {
                    return providers;
                }
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    })
    .await
    .expect("fournisseur attendu sous l'échéance")
}

/// Annonce par racine (ADR 0012) : après un `fetch_hls(Seed)` chez B,
/// `reprovide_all` ne réannonce que le manifeste ; C ne voit aucun
/// fournisseur pour un segment, mais récupère la publication via l'indice.
///
/// Les deux segments sont stockés directement sur le blockstore de A (voie
/// équivalente à `put_block`, `pub(crate)` et donc inaccessible depuis ce
/// test externe) — SANS jamais passer par `add`/`provide` : ainsi, la seule
/// façon dont un fournisseur pourrait apparaître pour un segment dans toute
/// cette DHT à 3 nœuds est un `reprovide_all` chez B qui aurait oublié de les
/// filtrer. Seul le manifeste, ajouté via `add`, est annoncé par A.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reprovide_all_announces_roots_only() {
    let dir = tempfile::tempdir().unwrap();

    // A : créateur. Segments stockés sans annonce, manifeste ajouté (annoncé).
    let kp_a = Keypair::generate_ed25519();
    let bs_a = Blockstore::open(dir.path().join("a")).unwrap();
    let node_a = Node::with_moderation(kp_a.clone(), bs_a, Moderation::empty())
        .await
        .unwrap();
    let addr_a = node_a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();

    let seg1 = node_a
        .blockstore()
        .put(b"premier segment du test reprovide_all")
        .unwrap();
    let seg2 = node_a
        .blockstore()
        .put(b"second segment du test reprovide_all")
        .unwrap();
    let manifest = HlsManifest::new(
        1.0,
        vec![
            HlsSegment {
                cid: seg1.to_string(),
                duration: 1.0,
            },
            HlsSegment {
                cid: seg2.to_string(),
                duration: 1.0,
            },
        ],
    );
    let manifest_cid = node_a
        .add(manifest.to_json().unwrap().as_bytes())
        .await
        .unwrap();

    // B : souscrit à A, fetch_hls en politique Seed → cache le manifeste ET
    // les 2 segments, mais n'annonce (au fil du fetch) que le manifeste
    // (annonce par racine, ADR 0012).
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

    node_b.subscribe(node_a.peer_id()).unwrap();
    let feed = Feed::build_signed(&kp_a, 1, &[manifest_cid]).unwrap();
    node_b.apply_feed_for_tests(feed).unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if node_b
                .catalog_subscribed()
                .iter()
                .any(|e| e.cids.contains(&manifest_cid))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("le catalogue souscrit de B doit voir le manifeste");

    let out = dir.path().join("hls-out");
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if node_b.fetch_hls(manifest_cid, &out).await.is_ok()
                && node_b.blockstore().has(&manifest_cid)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("fetch_hls doit finir par mettre le manifeste en cache (politique Seed)");

    assert!(node_b.blockstore().has(&seg1) && node_b.blockstore().has(&seg2));

    // B ne détient que cette publication : blockstore total (3 blocs :
    // manifeste + 2 segments) moins les 2 segments indexés par le SeedIndex
    // (peuplé par `fetch_hls` en Seed) = 1 root (le manifeste seul).
    let total_blocks = node_b.blockstore().list().unwrap().len();
    let expected_roots = total_blocks - 2;
    let reprovided = node_b.reprovide_all().await.unwrap();
    assert_eq!(
        reprovided, expected_roots,
        "reprovide_all doit réannoncer uniquement les roots (pas les segments)"
    );

    // C : connecté à B SEUL, jamais à A.
    let node_c = node(dir.path(), "c").await;
    node_c
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    node_c
        .add_address(node_b.peer_id(), addr_b.clone())
        .await
        .unwrap();
    node_c.dial(addr_b.clone()).await.unwrap();

    let manifest_providers = wait_for_provider(&node_c, manifest_cid, node_b.peer_id()).await;
    assert!(manifest_providers.contains(&node_b.peer_id()));

    let seg_providers = node_c.get_providers(seg1).await.unwrap();
    assert!(
        seg_providers.is_empty(),
        "aucun fournisseur ne doit être découvrable pour un segment depuis C : {seg_providers:?}"
    );

    // La publication reste néanmoins récupérable : les segments se
    // retrouvent via l'indice de racine sur les fournisseurs du manifeste.
    let out_c = dir.path().join("hls-out-c");
    let playlist = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(p) = node_c.fetch_hls(manifest_cid, &out_c).await {
                return p;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("fetch_hls doit finir par réussir chez C via l'indice de racine sur le manifeste");
    assert!(playlist.exists());
}
