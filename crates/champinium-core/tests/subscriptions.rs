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

/// Comme [`node`], mais avec un `follow_interval` de suivi actif explicite —
/// passé au constructeur (`with_moderation_and_follow_interval`), donc
/// effectif AVANT le `tokio::spawn` de `follow_loop`. Un setter
/// post-construction serait en course avec la toute première lecture de
/// l'intervalle par la boucle (aucun `.await` entre le spawn et cette
/// lecture) — course perdue en pratique sous charge CI (voir rapport), d'où
/// ce paramètre au constructeur plutôt qu'un setter.
async fn node_with_follow_interval(dir: &std::path::Path, name: &str, interval: Duration) -> Node {
    let kp = load_or_generate(dir.join(format!("{name}.key"))).unwrap();
    let bs = Blockstore::open(dir.join(name)).unwrap();
    Node::with_moderation_and_follow_interval(kp, bs, Moderation::empty(), interval)
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

    // Délai généreux (voir periodic_follow_picks_up_new_feed_versions) :
    // sous charge CI, un `timeout` trop serré flake sans que le comportement
    // testé soit en cause (juste le scheduler qui met plus longtemps à
    // exécuter les tâches réseau).
    tokio::time::timeout(Duration::from_secs(30), async {
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

/// Suivi actif (tâche 2), volet périodique : A publie v1 PUIS v2 avant toute
/// connexion à B — gossipsub ne rejoue jamais les messages passés à un pair
/// qui rejoint après coup, donc ni v1 ni v2 ne sont jamais gossipés à B, quoi
/// qu'il advienne ensuite. B se souscrit à A **avant** de se connecter : le
/// fetch immédiat déclenché par `subscribe()` échoue faute de route vers A à
/// cet instant et n'est **jamais retenté** (tâche unique, `tokio::spawn`
/// ponctuel) — il ne peut donc pas être la voie de convergence testée ici.
/// Une fois B connecté (v1/v2 déjà publiées, donc invisibles en gossip pour
/// lui), la SEULE voie de convergence restante est la boucle de fond
/// `follow_loop`, qui interroge la DHT à intervalle régulier (`follow_interval`).
///
/// Auto-vérification de l'isolation (voir rapport) : avec
/// `follow_interval` figé à 1 h, ce test échoue par timeout (aucune autre
/// voie ne peut livrer le feed) ; avec 100 ms, il passe.
///
/// `follow_interval` à 100 ms et un délai de convergence de 30 s (plutôt que
/// 10 s) : sous charge CI (runners partagés, threads contendus), 10 s s'est
/// avéré trop serré pour garantir plusieurs tentatives de la boucle
/// périodique dans la fenêtre — flake observé en CI (ubuntu-latest), pas en
/// local. Un intervalle plus court + un délai plus large font plus de
/// tentatives tenir dans la fenêtre sans changer ce que le test prouve.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn periodic_follow_picks_up_new_feed_versions() {
    let dir = tempfile::tempdir().unwrap();

    // A publie v1 puis v2 (PUT DHT + gossip dans le vide, personne d'autre
    // n'étant encore connecté) — seul le dernier PUT DHT (v2) survit sous la
    // clé `/champinium/feed/<peerid>` de A.
    let node_a = node(dir.path(), "a").await;
    node_a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    let c1 = cid_for(b"periodic v1");
    node_a.publish_feed(&[c1]).await.unwrap();
    let c2 = cid_for(b"periodic v2");
    node_a.publish_feed(&[c2]).await.unwrap();

    // B se souscrit à A alors qu'il n'est PAS encore connecté : le fetch
    // immédiat spawné par `subscribe()` échoue silencieusement (best-effort)
    // et ne repassera plus jamais. `follow_interval` (100 ms) est passé au
    // constructeur, donc effectif avant même le spawn de `follow_loop` —
    // voir `node_with_follow_interval`.
    let node_b = node_with_follow_interval(dir.path(), "b", Duration::from_millis(100)).await;
    node_b.subscribe(node_a.peer_id()).unwrap();

    // B ne se connecte à A qu'à présent, une fois v1 ET v2 déjà publiées.
    let addr_a = node_a.listen_addrs().await.unwrap();
    let addr_a = addr_a.into_iter().next().expect("adresse d'écoute de A");
    node_b
        .add_address(node_a.peer_id(), addr_a.clone())
        .await
        .unwrap();
    node_b.dial(addr_a).await.unwrap();

    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let subbed = node_b.catalog_subscribed();
            if subbed
                .iter()
                .any(|e| e.issuer == node_a.peer_id() && e.cids.contains(&c2))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("le suivi périodique doit retrouver la dernière version publiée par A");
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

        // Délai généreux (voir periodic_follow_picks_up_new_feed_versions) :
        // sous charge CI, 10 s s'est avéré trop serré.
        tokio::time::timeout(Duration::from_secs(30), async {
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
    // disque) SANS se resouscrire. `follow_interval` (200 ms plutôt que les
    // 5 min de la constante de production) est passé au constructeur — voir
    // `node_with_follow_interval` — donc effectif dès la toute première
    // itération de `follow_loop` : si celle-ci échoue (pas encore reconnecté
    // à A), la suivante arrive vite.
    let node_b = node_with_follow_interval(dir.path(), "b", Duration::from_millis(200)).await;
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

    // Délai généreux (voir periodic_follow_picks_up_new_feed_versions) : sous
    // charge CI, la fenêtre précédente (15 s) était plus courte que celle de
    // la passe périodique — élargie à 30 s pour symétrie.
    tokio::time::timeout(Duration::from_secs(30), async {
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
