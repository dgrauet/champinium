//! Durabilité du record de feed signé (défaut corrigé) : `champinium-seed` ne
//! réannonçait que les provider records de blocs (`reprovide_all`), jamais le
//! record de feed lui-même (`/champinium/feed/<peerid>`) — un créateur qui
//! publie puis passe hors ligne voyait son record s'éteindre au TTL du
//! `MemoryStore` Kademlia, cassant la découverte à froid de son dernier feed.
//! `Node::republish_known_feeds` corrige ça : un nœud réannonce le feed signé
//! qu'il détient légitimement (le sien, ceux de ses abonnements).
//!
//! Scénario à deux (puis trois) nœuds :
//! - A publie un feed (PUT DHT).
//! - S (un seeder) se connecte à A, s'abonne, puis republie (au moins un
//!   feed republié : le sien... non, A n'est pas lui — celui d'A, via
//!   l'abonnement).
//! - A est ABANDONNÉ (drop, hors ligne).
//! - N, un nœud tout neuf, se connecte SEULEMENT à S (jamais à A) et doit
//!   pouvoir `fetch_feed(A)` — preuve que la republication de S a maintenu le
//!   feed d'A découvrable sans qu'A soit en ligne.

use champinium_core::content::cid_for;
use champinium_core::identity::load_or_generate;
use champinium_core::moderation::{Denylist, Moderation};
use champinium_core::{Blockstore, Feed, Node};
use libp2p::identity::Keypair;
use std::time::Duration;

async fn node(dir: &std::path::Path, name: &str) -> Node {
    let kp = load_or_generate(dir.join(format!("{name}.key"))).unwrap();
    let bs = Blockstore::open(dir.join(name)).unwrap();
    Node::with_moderation(kp, bs, Moderation::empty())
        .await
        .unwrap()
}

/// Attend jusqu'à 30 s que `cond` devienne vraie, en pollant (pas de sleep
/// unique bloquant) — patron repris de `tests/subscriptions.rs`.
async fn wait_until<F, Fut>(mut cond: F, msg: &str)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if cond().await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect(msg);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn subscribed_seeder_republication_keeps_creator_feed_discoverable_offline() {
    let dir = tempfile::tempdir().unwrap();

    // A publie son feed (PUT DHT).
    let node_a = node(dir.path(), "a").await;
    node_a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    let c1 = cid_for(b"durability item 1");
    node_a.publish_feed(&[c1]).await.unwrap();
    let peer_a = node_a.peer_id();

    // S (seeder) se connecte à A, s'abonne (fetch immédiat inclus), converge,
    // puis republie explicitement — ceci doit republier AU MOINS un feed (le
    // feed d'A, désormais dans le catalogue de S via l'abonnement).
    let node_s = node(dir.path(), "s").await;
    let addr_a = node_a.listen_addrs().await.unwrap();
    let addr_a = addr_a.into_iter().next().expect("adresse d'écoute de A");
    node_s.add_address(peer_a, addr_a.clone()).await.unwrap();
    node_s.dial(addr_a).await.unwrap();
    node_s.subscribe(peer_a).unwrap();

    wait_until(
        || async {
            node_s
                .catalog_subscribed()
                .iter()
                .any(|e| e.issuer == peer_a)
        },
        "S doit converger sur le feed d'A avant de le republier",
    )
    .await;

    let republished = node_s.republish_known_feeds().await.unwrap();
    assert!(
        republished >= 1,
        "S doit republier au moins le feed souscrit d'A, obtenu {republished}"
    );

    // A est abandonné : hors ligne, plus aucune tentative de le joindre ne
    // peut aboutir.
    drop(node_a);

    // N, un nœud tout neuf, se connecte UNIQUEMENT à S (jamais à A) et doit
    // pouvoir retrouver le feed d'A dans la DHT — preuve que la republication
    // de S (et non une survivance fortuite du TTL) a maintenu le record
    // découvrable.
    let node_n = node(dir.path(), "n").await;
    let addr_s = node_s.listen_addrs().await.unwrap();
    // S n'écoutait pas explicitement : il faut le faire avant de connecter N.
    let addr_s = if addr_s.is_empty() {
        node_s
            .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .await
            .unwrap()
    } else {
        addr_s.into_iter().next().unwrap()
    };
    node_n
        .add_address(node_s.peer_id(), addr_s.clone())
        .await
        .unwrap();
    node_n.dial(addr_s).await.unwrap();

    wait_until(
        || async { node_n.fetch_feed(peer_a).await.map(|f| f.is_some()).unwrap_or(false) },
        "N doit retrouver le feed d'A via la DHT grâce à la republication de S, sans qu'A soit en ligne",
    )
    .await;

    let feed = node_n
        .fetch_feed(peer_a)
        .await
        .unwrap()
        .expect("feed d'A trouvé");
    assert!(feed.cids().unwrap().contains(&c1));
}

/// `republish_known_feeds` ne republie QUE les abonnements (jamais tout le
/// catalogue) : un émetteur présent au catalogue mais NON souscrit est
/// ignoré.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn republish_known_feeds_skips_non_subscribed_catalog_issuer() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(dir.path()).await.unwrap();

    let other = Keypair::generate_ed25519();
    let feed = Feed::build_signed(&other, 1, &[cid_for(b"unsubscribed")]).unwrap();
    node.apply_feed_for_tests(feed).unwrap();
    // Pas d'abonnement à `other` : présent au catalogue, mais pas souscrit.

    let count = node.republish_known_feeds().await.unwrap();
    assert_eq!(
        count, 0,
        "un émetteur au catalogue mais non souscrit ne doit pas être republié"
    );
}

/// Un émetteur SOUSCRIT mais BLOQUÉ (banni par denylist) reste exclu de la
/// republication — cohérent avec la modération : on ne republie pas le feed
/// d'une clé qu'on refuse par ailleurs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn republish_known_feeds_skips_blocked_issuer() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(dir.path()).await.unwrap();

    let victim = Keypair::generate_ed25519();
    let victim_peer = victim.public().to_peer_id();

    // Abonnement AVANT le bannissement (sinon `subscribe` refuse déjà).
    node.subscribe(victim_peer).unwrap();

    // Bannissement par denylist signée : purge rétroactive le catalogue (rien
    // à purger ici, `victim` n'y est pas encore) mais laisse l'abonnement
    // intact (`purge_blocked_issuer` ne touche pas `subscriptions`).
    let signer = Keypair::generate_ed25519();
    let list =
        Denylist::build_signed("test-list", "2026-07-23", &signer, &[], &[victim_peer]).unwrap();
    node.subscribe_denylist(&list).await.unwrap();

    // Réinjection directe du feed au catalogue (test uniquement, bypasse la
    // modération à l'ingestion) pour isoler le filtre de
    // `republish_known_feeds` de celui, déjà couvert ailleurs, de
    // `Catalog::apply`/`fetch_feed_inner`.
    let feed = Feed::build_signed(&victim, 1, &[cid_for(b"blocked")]).unwrap();
    node.apply_feed_for_tests(feed).unwrap();

    let count = node.republish_known_feeds().await.unwrap();
    assert_eq!(
        count, 0,
        "un émetteur souscrit mais banni ne doit jamais être republié"
    );
}
