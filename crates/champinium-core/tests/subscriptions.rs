//! Abonnements : liste locale persistée (spec channels §2). État PRIVÉ du
//! nœud — jamais publié sur le réseau.

use champinium_core::content::cid_for;
use champinium_core::identity::load_or_generate;
use champinium_core::{Blockstore, Moderation, Node};
use libp2p::identity::Keypair;
use std::time::Duration;

async fn node(dir: &std::path::Path, name: &str) -> Node {
    let kp = load_or_generate(dir.join(format!("{name}.key"))).unwrap();
    let bs = Blockstore::open(dir.join(name)).unwrap();
    Node::with_moderation(kp, bs, Moderation::empty())
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscriptions_persist_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let issuer = Keypair::generate_ed25519().public().to_peer_id();
    {
        let node = Node::open(dir.path()).await.unwrap();
        node.subscribe(issuer).unwrap();
        assert_eq!(node.subscriptions(), vec![issuer]);
    }
    let node = Node::open(dir.path()).await.unwrap();
    assert_eq!(node.subscriptions(), vec![issuer]);

    node.unsubscribe(issuer).unwrap();
    assert!(node.subscriptions().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_subscribed_filters_to_followed_issuers() {
    // Deux feeds dans le catalogue (via gossip local publish + apply direct) ;
    // seul l'émetteur souscrit apparaît dans catalog_subscribed().
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(dir.path()).await.unwrap();

    // Mon propre feed (non souscrit) + un feed tiers appliqué à la main.
    let cid = champinium_core::content::cid_for(b"x");
    node.publish_feed(&[cid]).await.unwrap();

    let other = Keypair::generate_ed25519();
    let feed = champinium_core::Feed::build_signed(&other, 1, &[cid]).unwrap();
    node.apply_feed_for_tests(feed).unwrap(); // voir note step 5

    node.subscribe(other.public().to_peer_id()).unwrap();
    let subbed = node.catalog_subscribed();
    assert_eq!(subbed.len(), 1);
    assert_eq!(subbed[0].issuer, other.public().to_peer_id());
    assert_eq!(node.catalog_entries().len(), 2, "Explorer voit tout");
}

/// Suivi actif (tâche 2) : B se souscrit à A sans qu'aucun gossip n'ait
/// circulé (A publie AVANT que B ne se connecte, comme dans
/// `feed_dht.rs::feed_is_discoverable_via_dht_without_gossip`) — le fetch
/// immédiat déclenché par `subscribe()` doit retrouver le feed via la DHT.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_triggers_immediate_dht_fetch() {
    let dir = tempfile::tempdir().unwrap();

    // A publie son feed (PUT DHT) avant toute connexion : pas de gossip vers B.
    let node_a = node(dir.path(), "a").await;
    node_a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    let c1 = cid_for(b"follow item 1");
    node_a.publish_feed(&[c1]).await.unwrap();

    // B se connecte ensuite (toujours pas de gossip reçu pour ce feed déjà
    // publié) puis se souscrit à A.
    let node_b = node(dir.path(), "b").await;
    let addr_a = node_a.listen_addrs().await.unwrap();
    let addr_a = addr_a.into_iter().next().expect("adresse d'écoute de A");
    node_b
        .add_address(node_a.peer_id(), addr_a.clone())
        .await
        .unwrap();
    node_b.dial(addr_a).await.unwrap();

    node_b.subscribe(node_a.peer_id()).unwrap();

    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let subbed = node_b.catalog_subscribed();
            if subbed.iter().any(|e| e.issuer == node_a.peer_id()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("le fetch immédiat à l'abonnement doit retrouver le feed de A");
}

/// Suivi actif (tâche 2), volet périodique : une fois B convergé sur la v1
/// (fetch immédiat au subscribe), A publie une v2 SANS que B ne se resouscrive
/// — seule la boucle de fond `follow_loop` (relance toutes les
/// `follow_interval`) doit retrouver la nouvelle version.
///
/// `set_follow_interval_for_tests` est appelé au tout début, avant tout
/// `.await` réseau (add_address/dial/subscribe) : `follow_interval` est lu à
/// chaque itération de la boucle (pas figé à la création), mais un `sleep`
/// déjà lancé ne raccourcit pas rétroactivement — il faut donc gagner la
/// course contre la toute première itération de la boucle (souscriptions
/// vides à cet instant, donc son passage est un no-op quoi qu'il arrive) pour
/// que le *sommeil* qui la suit soit déjà court. En pratique la fenêtre est
/// large : le code synchrone qui suit (aucun réseau avant le premier `.await`)
/// s'exécute largement avant qu'un thread worker ne puisse même être réveillé
/// pour exécuter la tâche fraîchement spawnée.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn periodic_follow_picks_up_new_feed_versions() {
    let dir = tempfile::tempdir().unwrap();

    // A publie une première version de son feed (PUT DHT).
    let node_a = node(dir.path(), "a").await;
    node_a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    let c1 = cid_for(b"periodic v1");
    node_a.publish_feed(&[c1]).await.unwrap();

    let node_b = node(dir.path(), "b").await;
    node_b.set_follow_interval_for_tests(Duration::from_millis(200));

    let addr_a = node_a.listen_addrs().await.unwrap();
    let addr_a = addr_a.into_iter().next().expect("adresse d'écoute de A");
    node_b
        .add_address(node_a.peer_id(), addr_a.clone())
        .await
        .unwrap();
    node_b.dial(addr_a).await.unwrap();
    node_b.subscribe(node_a.peer_id()).unwrap();

    // Convergence initiale sur la v1 (fetch immédiat au subscribe).
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let subbed = node_b.catalog_subscribed();
            if subbed
                .iter()
                .any(|e| e.issuer == node_a.peer_id() && e.cids.contains(&c1))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("convergence initiale sur la v1 attendue");

    // A publie une v2 (seq supérieur, CID différent) — B ne se resouscrit pas.
    let c2 = cid_for(b"periodic v2");
    node_a.publish_feed(&[c2]).await.unwrap();

    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let subbed = node_b.catalog_subscribed();
            if subbed.iter().any(|e| {
                e.issuer == node_a.peer_id() && e.cids.contains(&c2) && !e.cids.contains(&c1)
            }) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("le suivi périodique doit retrouver la nouvelle version publiée par A");
}

/// Suivi actif (tâche 2), volet rattrapage au démarrage : B se souscrit à A,
/// converge, puis les deux nœuds s'arrêtent. A redémarre et publie une v2
/// PENDANT que B est hors ligne. B redémarre (même `data_dir` → abonnement
/// rechargé depuis le disque) et se reconnecte à A **sans jamais rappeler
/// `subscribe`** : seule la toute première itération de `follow_loop` (qui a
/// lieu avant tout `sleep`, précisément pour couvrir ce cas — spec §2) doit
/// retrouver la v2.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_initial_pass_catches_up_offline_updates() {
    let dir = tempfile::tempdir().unwrap();
    let c1 = cid_for(b"startup v1");

    // Session 1 : A publie v1, B se souscrit et converge, puis les deux
    // s'arrêtent proprement (fin de portée → drop des `Node` → `alive` tombe
    // à zéro → les boucles de fond s'arrêtent, cf. commentaire sur `alive`).
    {
        let node_a = node(dir.path(), "a").await;
        node_a
            .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .await
            .unwrap();
        node_a.publish_feed(&[c1]).await.unwrap();

        let node_b = node(dir.path(), "b").await;
        let addr_a = node_a.listen_addrs().await.unwrap();
        let addr_a = addr_a.into_iter().next().expect("adresse d'écoute de A");
        node_b
            .add_address(node_a.peer_id(), addr_a.clone())
            .await
            .unwrap();
        node_b.dial(addr_a).await.unwrap();
        node_b.subscribe(node_a.peer_id()).unwrap();

        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let subbed = node_b.catalog_subscribed();
                if subbed
                    .iter()
                    .any(|e| e.issuer == node_a.peer_id() && e.cids.contains(&c1))
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("convergence initiale sur la v1 attendue avant l'arrêt");
    }

    // A redémarre (même clé → même PeerId) et publie une v2 hors ligne (B pas
    // encore reconnecté).
    let node_a = node(dir.path(), "a").await;
    node_a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    let c2 = cid_for(b"startup v2");
    node_a.publish_feed(&[c2]).await.unwrap();

    // B redémarre (même répertoire → abonnement à A rechargé depuis le
    // disque) SANS se resouscrire. Accélère l'intervalle avant tout `.await`
    // réseau : si la toute première tentative de la passe initiale échoue
    // (pas encore reconnecté à A), la suivante arrive vite plutôt qu'après les
    // 5 min de la constante de production (voir set_follow_interval_for_tests).
    let node_b = node(dir.path(), "b").await;
    node_b.set_follow_interval_for_tests(Duration::from_millis(200));
    assert_eq!(
        node_b.subscriptions(),
        vec![node_a.peer_id()],
        "l'abonnement à A doit avoir survécu au redémarrage"
    );

    let addr_a = node_a.listen_addrs().await.unwrap();
    let addr_a = addr_a.into_iter().next().expect("adresse d'écoute de A");
    node_b
        .add_address(node_a.peer_id(), addr_a.clone())
        .await
        .unwrap();
    node_b.dial(addr_a).await.unwrap();

    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let subbed = node_b.catalog_subscribed();
            if subbed
                .iter()
                .any(|e| e.issuer == node_a.peer_id() && e.cids.contains(&c2))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect(
        "le rattrapage au démarrage doit retrouver la v2 publiée pendant que B était hors ligne",
    );
}
