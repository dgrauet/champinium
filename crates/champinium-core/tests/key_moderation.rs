//! Modération par CLÉ (lot channels d, tâches 2/3 — finding M2 de lot c).
//!
//! Tâche 2 : une clé entière peut être bannie (denylist v2, `key_entries`) —
//! ses feeds sont rejetés à l'ingestion du catalogue (gossip, DHT/`fetch_feed`),
//! et une souscription à chaud purge rétroactivement ce que ce nœud avait
//! lui-même ATTRIBUÉ à cette clé (son entrée de catalogue, son SeedIndex) —
//! jamais du contenu tiers simplement mentionné par elle (voir la revue
//! post-implémentation ci-dessous). Le checkpoint #2/service (`get_with`,
//! `handle_block_message`) reste un check CID direct (`moderation.is_blocked`),
//! inchangé par cette tâche.
//!
//! Tâche 3 : un channel peut aussi être bloqué LOCALEMENT (préférence privée
//! d'un nœud, jamais publiée) — même mécanisme d'ingestion catalogue et de
//! purge attribuée, en plus des pins.
//!
//! **Revue post-implémentation (finding critique I2)** : une première version
//! captait passivement, dans un registre dérivé, les CIDs *listés* par le
//! feed d'une clé bannie, puis les bloquait au checkpoint #2/service. Ce
//! mécanisme a été retiré : lister un CID ne prouve rien sur qui le détient
//! réellement, une clé bannie aurait donc pu faire disparaître le contenu
//! d'un tiers innocent en le mentionnant dans son propre feed (censure par
//! injection) — voir `blocked_key_cannot_censor_an_innocent_third_party_cid_by_listing_it`
//! ci-dessous. La purge se limite désormais à ce que ce nœud peut réellement
//! attribuer à la clé bannie.

use champinium_core::content::cid_for;
use champinium_core::identity::load_or_generate;
use champinium_core::{Blockstore, CoreError, Denylist, Feed, Moderation, Node};
use libp2p::identity::Keypair;
use libp2p::PeerId;
use std::time::Duration;

const UPDATED: &str = "2026-07-23T00:00:00Z";

async fn node_with(dir: &std::path::Path, name: &str, moderation: Moderation) -> Node {
    let kp = load_or_generate(dir.join(format!("{name}.key"))).unwrap();
    let bs = Blockstore::open(dir.join(name)).unwrap();
    Node::with_moderation(kp, bs, moderation).await.unwrap()
}

async fn node(dir: &std::path::Path, name: &str) -> Node {
    node_with(dir, name, Moderation::empty()).await
}

/// Denylist v2 signée bannissant une clé entière (aucun CID direct).
fn moderation_blocking_key(issuer_key: &std::path::Path, blocked: PeerId) -> Moderation {
    let issuer = load_or_generate(issuer_key).unwrap();
    let dl = Denylist::build_signed("test", UPDATED, &issuer, &[], &[blocked]).unwrap();
    let mut m = Moderation::empty();
    m.subscribe(&dl).unwrap();
    m
}

async fn connect(a: &Node, b: &Node) {
    let addr = a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    b.add_address(a.peer_id(), addr.clone()).await.unwrap();
    b.dial(addr).await.unwrap();
}

/// #1 catalogue (gossip) : le feed d'un émetteur banni par CLÉ n'entre jamais
/// au catalogue, même signature valide — un émetteur sain, lui, circule
/// normalement (pas de collatéral).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gossip_feed_from_key_blocked_issuer_is_rejected_from_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let node_bad = node(dir.path(), "bad").await;
    let node_clean = node(dir.path(), "clean").await;

    let moderation = moderation_blocking_key(&dir.path().join("issuer.key"), node_bad.peer_id());
    let node_b = node_with(dir.path(), "b", moderation).await;

    connect(&node_bad, &node_b).await;
    connect(&node_clean, &node_b).await;

    let bad_cid = cid_for(b"contenu d'une cle bannie");
    let clean_cid = cid_for(b"contenu sain");

    // Republie tant que le mesh gossipsub n'est pas formé (heartbeat ~1 s) —
    // même patron que `feed_gossip.rs::signed_feed_propagates_via_gossipsub`.
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            node_bad.publish_feed(&[bad_cid]).await.unwrap();
            node_clean.publish_feed(&[clean_cid]).await.unwrap();
            if node_b
                .catalog_entries()
                .iter()
                .any(|e| e.issuer == node_clean.peer_id())
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .expect("l'émetteur sain doit apparaître au catalogue");

    // Fenêtre d'observation : l'émetteur banni ne doit JAMAIS apparaître.
    for _ in 0..15 {
        assert!(
            !node_b
                .catalog_entries()
                .iter()
                .any(|e| e.issuer == node_bad.peer_id()),
            "un émetteur banni par clé ne doit jamais entrer au catalogue"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// #1 catalogue (DHT / `fetch_feed`) : même refus via la découverte hors
/// gossip — `fetch_feed` renvoie `None` plutôt que le feed trouvé.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fetch_feed_from_key_blocked_issuer_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    let node_bad = node(dir.path(), "bad").await;
    node_bad
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    let bad_cid = cid_for(b"dht cle bannie");
    node_bad.publish_feed(&[bad_cid]).await.unwrap();

    let moderation = moderation_blocking_key(&dir.path().join("issuer.key"), node_bad.peer_id());
    let node_b = node_with(dir.path(), "b", moderation).await;
    let addr = node_bad.listen_addrs().await.unwrap().remove(0);
    node_b
        .add_address(node_bad.peer_id(), addr.clone())
        .await
        .unwrap();
    node_b.dial(addr).await.unwrap();

    // Le PUT DHT a pu prendre un instant : réessaie jusqu'à convergence du
    // verdict "aucun feed exploitable" (None), jamais Some.
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let result = node_b.fetch_feed(node_bad.peer_id()).await.unwrap();
            if result.is_none()
                && !node_b
                    .catalog_entries()
                    .iter()
                    .any(|e| e.issuer == node_bad.peer_id())
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    })
    .await
    .expect("fetch_feed doit finir par renvoyer None pour une clé bannie");
}

/// Régression anti-injection (finding critique I2, revue post-implémentation) :
/// une clé bannie ne peut PAS faire disparaître le contenu d'un tiers innocent
/// en le listant dans son propre feed. Le mécanisme de capture passive des
/// CIDs "connus" d'une clé bloquée a été retiré précisément pour ça — lister
/// un CID ne prouve rien sur qui le détient réellement.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocked_key_cannot_censor_an_innocent_third_party_cid_by_listing_it() {
    let dir = tempfile::tempdir().unwrap();

    // Le tiers innocent possède réellement le contenu et le sert.
    let node_victim = node(dir.path(), "victim").await;
    let content = b"contenu innocent d'un tiers".to_vec();
    let victim_cid = node_victim.add(&content).await.unwrap();

    // La clé bannie ne possède PAS ce contenu — elle se contente de le
    // mentionner dans son propre feed (injection).
    let node_bad = node(dir.path(), "bad").await;

    let moderation = moderation_blocking_key(&dir.path().join("issuer.key"), node_bad.peer_id());
    let node_b = node_with(dir.path(), "b", moderation).await;
    connect(&node_victim, &node_b).await;
    connect(&node_bad, &node_b).await;

    // Republie tant que le mesh gossipsub n'est pas formé, pour être sûr que
    // le feed d'injection a bien été vu (et rejeté) par `node_b` avant
    // d'affirmer que son injection est restée sans effet.
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            node_bad.publish_feed(&[victim_cid]).await.unwrap();
            if !node_b
                .catalog_entries()
                .iter()
                .any(|e| e.issuer == node_bad.peer_id())
            {
                // Le feed d'injection est bien rejeté (attendu) ; mais le CID
                // du tiers doit rester récupérable dès maintenant.
                if let Ok(bytes) = node_b.get(victim_cid).await {
                    if bytes == content {
                        return;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .expect("le CID d'un tiers innocent doit rester récupérable malgré l'injection");
}

/// Mirroir du patron lot (c) (`unsubscribe_keeps_segment_shared_with_a_pinned_publication`) :
/// un segment partagé entre la publication d'un émetteur bloqué et celle d'un
/// AUTRE émetteur non bloqué survit à `purge_blocked_issuer` (finding I1).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn purge_blocked_issuer_keeps_segment_shared_with_another_issuer() {
    let dir = tempfile::tempdir().unwrap();

    let node_a = node(dir.path(), "a").await; // sera bloqué
    let node_b_creator = node(dir.path(), "bcreator").await; // reste sain

    let shared_bytes = b"segment partage entre deux emetteurs";
    let shared_cid_a = node_a.add(shared_bytes).await.unwrap();
    let shared_cid_b = node_b_creator.add(shared_bytes).await.unwrap();
    assert_eq!(shared_cid_a, shared_cid_b, "contenu identique -> même CID");

    // Chaque manifeste porte AUSSI un segment propre à son émetteur, pour que
    // les deux manifestes (donc leurs CIDs) restent distincts malgré le
    // segment partagé — sinon un manifeste identique en tout point produirait
    // le MÊME CID des deux côtés (contenu adressé), rendant "le manifeste de
    // l'émetteur sain survit" indissociable de "le segment partagé survit".
    let unique_a = node_a
        .add(b"segment propre a l'emetteur bloque")
        .await
        .unwrap();
    let manifest_a = champinium_core::HlsManifest::new(
        4.0,
        vec![
            champinium_core::ingest::HlsSegment {
                cid: unique_a.to_string(),
                duration: 4.0,
            },
            champinium_core::ingest::HlsSegment {
                cid: shared_cid_a.to_string(),
                duration: 4.0,
            },
        ],
    );
    let manifest_cid_a = node_a
        .add(manifest_a.to_json().unwrap().as_bytes())
        .await
        .unwrap();
    node_a.publish_feed(&[manifest_cid_a]).await.unwrap();

    let unique_b = node_b_creator
        .add(b"segment propre a l'emetteur sain")
        .await
        .unwrap();
    let manifest_b = champinium_core::HlsManifest::new(
        4.0,
        vec![
            champinium_core::ingest::HlsSegment {
                cid: unique_b.to_string(),
                duration: 4.0,
            },
            champinium_core::ingest::HlsSegment {
                cid: shared_cid_b.to_string(),
                duration: 4.0,
            },
        ],
    );
    let manifest_cid_b = node_b_creator
        .add(manifest_b.to_json().unwrap().as_bytes())
        .await
        .unwrap();
    node_b_creator
        .publish_feed(&[manifest_cid_b])
        .await
        .unwrap();
    assert_ne!(manifest_cid_a, manifest_cid_b);

    let node_test = node(dir.path(), "test").await;
    connect(&node_a, &node_test).await;
    connect(&node_b_creator, &node_test).await;
    node_test.subscribe(node_a.peer_id()).unwrap();
    node_test.subscribe(node_b_creator.peer_id()).unwrap();

    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let _ = node_test.fetch_feed(node_a.peer_id()).await;
            let _ = node_test.fetch_feed(node_b_creator.peer_id()).await;
            if node_test.blockstore().has(&manifest_cid_a)
                && node_test.blockstore().has(&manifest_cid_b)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    })
    .await
    .expect("le seed proactif doit retenir les deux publications avant le blocage");

    let issuer = load_or_generate(dir.path().join("issuer.key")).unwrap();
    let dl = Denylist::build_signed("test", UPDATED, &issuer, &[], &[node_a.peer_id()]).unwrap();
    node_test.subscribe_denylist(&dl).await.unwrap();

    assert!(
        !node_test.blockstore().has(&manifest_cid_a),
        "le manifeste de l'émetteur bloqué doit être purgé"
    );
    assert!(
        node_test.blockstore().has(&manifest_cid_b),
        "le manifeste de l'émetteur sain doit survivre"
    );
    assert!(
        node_test.blockstore().has(&shared_cid_a),
        "le segment PARTAGÉ doit survivre — encore référencé par l'émetteur sain"
    );
    assert!(
        !node_test.blockstore().has(&unique_a),
        "le segment propre à l'émetteur bloqué doit être purgé"
    );
    assert!(
        node_test.blockstore().has(&unique_b),
        "le segment propre à l'émetteur sain doit survivre"
    );
}

/// Souscription à chaud d'une denylist à clé : purge rétroactive de
/// l'entrée de catalogue ET du stock seedé (SeedIndex + blockstore),
/// **pins compris** — la modération prime sur les pins. Stats cohérentes
/// (le quota utilisé retombe à zéro pour ce qui a été purgé).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_denylist_purges_catalog_and_seeded_stock_including_pins() {
    let dir = tempfile::tempdir().unwrap();

    // Émetteur qui publie un manifeste HLS minimal (construit à la main,
    // pas d'ffmpeg réel nécessaire ici — seul le seed proactif de B doit le
    // reconnaître comme tel).
    let node_bad = node(dir.path(), "bad").await;
    let seg_cid = node_bad.add(b"segment epingle par b").await.unwrap();
    let manifest = champinium_core::HlsManifest::new(
        4.0,
        vec![champinium_core::ingest::HlsSegment {
            cid: seg_cid.to_string(),
            duration: 4.0,
        }],
    );
    let manifest_cid = node_bad
        .add(manifest.to_json().unwrap().as_bytes())
        .await
        .unwrap();
    node_bad.publish_feed(&[manifest_cid]).await.unwrap();

    // B souscrit ce channel : le seed proactif retient la publication sous
    // quota (pas épinglé automatiquement chez B — seul le créateur
    // auto-épingle) ; on épingle explicitement chez B pour couvrir "pins
    // compris" (tâche 2 exige que la purge par clé outrepasse les pins).
    let node_b = node(dir.path(), "b").await;
    connect(&node_bad, &node_b).await;
    node_b.subscribe(node_bad.peer_id()).unwrap();

    // Réessaie `fetch_feed` (best-effort, DHT) pour ne pas dépendre d'une
    // seule course avec le PUT DHT de `publish_feed` — une fois le feed au
    // catalogue, le seed proactif (réveillé par `catalog_events`) retient la
    // publication.
    // Attendre que la publication soit **indexée au SeedIndex**, pas seulement
    // que ses blocs soient sur disque : `seed_publication` insère à l'index
    // APRÈS avoir écrit les blocs, donc `blockstore().has()` peut être vrai
    // alors que `storage_stats().used` (qui lit le SeedIndex) vaut encore 0 —
    // fenêtre élargie sous charge parallèle (flake CI observé). `pin` opère
    // aussi sur l'index, il ne prend effet qu'une fois la publication indexée.
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let _ = node_b.fetch_feed(node_bad.peer_id()).await;
            if node_b.storage_stats().0 > 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    })
    .await
    .expect("le seed proactif doit retenir la publication du channel souscrit");
    node_b.pin(manifest_cid).unwrap();
    let (used_before, _quota) = node_b.storage_stats();
    assert!(used_before > 0, "le stock seedé doit être comptabilisé");

    // Souscription à une denylist bannissant la clé de `node_bad`.
    let moderation_issuer = load_or_generate(dir.path().join("issuer.key")).unwrap();
    let dl = Denylist::build_signed(
        "test",
        UPDATED,
        &moderation_issuer,
        &[],
        &[node_bad.peer_id()],
    )
    .unwrap();
    let purged = node_b.subscribe_denylist(&dl).await.unwrap();

    assert!(purged > 0, "au moins le manifeste doit être purgé");
    assert!(
        !node_b
            .catalog_entries()
            .iter()
            .any(|e| e.issuer == node_bad.peer_id()),
        "l'entrée de catalogue doit être purgée"
    );
    assert!(
        !node_b.blockstore().has(&manifest_cid),
        "le bloc épinglé doit quand même être purgé — la modération prime sur les pins"
    );
    let (used_after, _quota) = node_b.storage_stats();
    assert_eq!(
        used_after, 0,
        "le SeedIndex doit refléter la purge (stats cohérentes)"
    );
    // Depuis le retrait de la capture passive (finding I2), la purge par clé
    // est purement locale/attribuée — PAS une interdiction du CID au niveau
    // réseau : `get` peut donc encore récupérer ce même contenu si un pair le
    // sert toujours (ici l'émetteur bloqué lui-même, qui n'est pas modéré
    // vis-à-vis de son propre contenu). Ça prouve que la purge n'agit pas
    // comme une modération par CID — seul `moderation.is_blocked` (denylist
    // CID directe) ferait refuser durablement ce contenu.
    let refetched = node_b
        .get(manifest_cid)
        .await
        .expect("le contenu n'est pas bloqué par CID, seulement purgé localement");
    assert!(!refetched.is_empty());
}

/// Cas sain : une denylist à clé qui ne concerne personne du catalogue
/// courant n'a aucun effet collatéral (comportement CID historique préservé).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_denylist_does_not_affect_unrelated_issuer() {
    let dir = tempfile::tempdir().unwrap();
    let node_clean = node(dir.path(), "clean").await;
    let node_b = node(dir.path(), "b").await;
    connect(&node_clean, &node_b).await;

    let clean_cid = cid_for(b"contenu sain, sans rapport");

    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            node_clean.publish_feed(&[clean_cid]).await.unwrap();
            if node_b
                .catalog_entries()
                .iter()
                .any(|e| e.issuer == node_clean.peer_id())
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .expect("l'émetteur sain doit converger avant la souscription");

    // Denylist à clé visant un tiers totalement étranger au catalogue.
    let unrelated = Keypair::generate_ed25519().public().to_peer_id();
    let issuer = load_or_generate(dir.path().join("issuer.key")).unwrap();
    let dl = Denylist::build_signed("test", UPDATED, &issuer, &[], &[unrelated]).unwrap();
    let purged = node_b.subscribe_denylist(&dl).await.unwrap();

    assert_eq!(purged, 0);
    assert!(node_b
        .catalog_entries()
        .iter()
        .any(|e| e.issuer == node_clean.peer_id()));
}

/// Un feed invalide (non signé par la clé revendiquée) ne doit jamais être
/// exploitable pour un check de clé bloquée : `feed.verify()` a lieu AVANT
/// toute lecture de `issuer_pubkey`/`issuer_peer_id()` aux points d'entrée du
/// catalogue (`handle_feed_message`, `fetch_feed_inner`).
#[test]
fn unsigned_feed_forging_a_blocked_issuer_pubkey_does_not_verify() {
    let real_issuer = Keypair::generate_ed25519();
    let forger = Keypair::generate_ed25519();
    let cid = cid_for(b"forgerie");
    let mut feed = Feed::build_signed(&forger, 1, &[cid]).unwrap();
    // Falsifie la clé publique déclarée pour viser la clé de `real_issuer`
    // sans posséder sa clé privée : la signature ne peut plus vérifier.
    feed.issuer_pubkey = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        real_issuer.public().encode_protobuf(),
    );
    assert!(
        feed.verify().is_err(),
        "signature falsifiée: verify() doit échouer"
    );
}

// ---------------------------------------------------------------------
// Tâche 3 : blocage LOCAL de channel — invisible partout pour ce nœud,
// jamais publié, purement local (pas de rapport, pas de record réseau —
// vérifié par relecture du code de `Node::block_channel`/`purge_blocked_
// issuer` : aucun appel à `emit_report_inner`/`Command::PublishReport`).
// ---------------------------------------------------------------------

/// Bloquer localement : catalogue purgé, désabonné, stock (SeedIndex +
/// blockstore, pins compris) purgé.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn block_channel_purges_catalog_unsubscribes_and_purges_stock() {
    let dir = tempfile::tempdir().unwrap();
    let node_a = node(dir.path(), "a").await;
    let seg_cid = node_a.add(b"segment local block").await.unwrap();
    let manifest = champinium_core::HlsManifest::new(
        4.0,
        vec![champinium_core::ingest::HlsSegment {
            cid: seg_cid.to_string(),
            duration: 4.0,
        }],
    );
    let manifest_cid = node_a
        .add(manifest.to_json().unwrap().as_bytes())
        .await
        .unwrap();
    node_a.publish_feed(&[manifest_cid]).await.unwrap();

    let node_b = node(dir.path(), "b").await;
    connect(&node_a, &node_b).await;
    node_b.subscribe(node_a.peer_id()).unwrap();

    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let _ = node_b.fetch_feed(node_a.peer_id()).await;
            if node_b.blockstore().has(&manifest_cid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    })
    .await
    .expect("le seed proactif doit retenir la publication avant le blocage");
    node_b.pin(manifest_cid).unwrap();
    assert!(node_b.subscriptions().contains(&node_a.peer_id()));

    node_b.block_channel(node_a.peer_id()).await.unwrap();

    assert!(
        !node_b.subscriptions().contains(&node_a.peer_id()),
        "le blocage doit désabonner"
    );
    assert!(
        !node_b
            .catalog_entries()
            .iter()
            .any(|e| e.issuer == node_a.peer_id()),
        "l'entrée de catalogue doit être purgée"
    );
    assert!(
        !node_b.blockstore().has(&manifest_cid),
        "le stock doit être purgé, pins compris"
    );
    let (used, _quota) = node_b.storage_stats();
    assert_eq!(used, 0);
    assert_eq!(node_b.blocked_channels(), vec![node_a.peer_id()]);
}

/// Après blocage, les feeds SUIVANTS du channel bloqué sont rejetés du
/// catalogue (même mécanisme que la tâche 2, ensembles fusionnés), et
/// `subscribe` y est désormais refusé avec une erreur claire.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn block_channel_rejects_future_feeds_and_refuses_subscribe() {
    let dir = tempfile::tempdir().unwrap();
    let node_a = node(dir.path(), "a").await;
    let node_b = node(dir.path(), "b").await;
    connect(&node_a, &node_b).await;

    node_b.block_channel(node_a.peer_id()).await.unwrap();

    let err = node_b.subscribe(node_a.peer_id()).unwrap_err();
    assert!(
        matches!(err, CoreError::Moderated(msg) if msg.contains("bloqué localement")),
        "message clair attendu, et Moderated (pas Moderation) pour que le contrat FFI v8 \
         remonte FfiError::Moderated plutôt qu'InvalidInput"
    );

    let cid = cid_for(b"feed apres blocage local");
    for _ in 0..15 {
        node_a.publish_feed(&[cid]).await.unwrap();
        assert!(
            !node_b
                .catalog_entries()
                .iter()
                .any(|e| e.issuer == node_a.peer_id()),
            "un feed émis après blocage local ne doit jamais entrer au catalogue"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Débloquer : le contenu revient naturellement à la prochaine réception
/// (gossip/DHT), sans action supplémentaire.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unblock_channel_lets_feed_return_via_gossip() {
    let dir = tempfile::tempdir().unwrap();
    let node_a = node(dir.path(), "a").await;
    let node_b = node(dir.path(), "b").await;
    connect(&node_a, &node_b).await;

    node_b.block_channel(node_a.peer_id()).await.unwrap();
    node_b.unblock_channel(node_a.peer_id()).unwrap();
    assert!(node_b.blocked_channels().is_empty());

    let cid = cid_for(b"feed apres deblocage");
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            node_a.publish_feed(&[cid]).await.unwrap();
            if node_b
                .catalog_entries()
                .iter()
                .any(|e| e.issuer == node_a.peer_id())
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .expect("le feed doit revenir naturellement après déblocage");
}

/// Persistance au redémarrage (même patron que
/// `subscriptions.rs::subscriptions_persist_across_restart`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocked_channels_persist_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let target = Keypair::generate_ed25519().public().to_peer_id();
    {
        let node = Node::open(dir.path()).await.unwrap();
        node.block_channel(target).await.unwrap();
        assert_eq!(node.blocked_channels(), vec![target]);
    }
    let node = Node::open(dir.path()).await.unwrap();
    assert_eq!(node.blocked_channels(), vec![target]);

    node.unblock_channel(target).unwrap();
    assert!(node.blocked_channels().is_empty());
}
