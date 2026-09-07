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

/// Comme [`node`], mais avec des boucles de fond rapides (suivi et seed) —
/// même patron que `tests/proactive_seed.rs`. La maintenance reste à sa
/// valeur de production : ces tests ne la mesurent pas.
async fn fast_node(dir: &std::path::Path, name: &str) -> Node {
    let kp = load_or_generate(dir.join(format!("{name}.key"))).unwrap();
    let bs = Blockstore::open(dir.join(name)).unwrap();
    Node::with_moderation_and_intervals(
        kp,
        bs,
        Moderation::empty(),
        Duration::from_millis(100),
        Duration::from_millis(100),
        champinium_core::p2p::REPROVIDE_INTERVAL,
        None,
    )
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

/// Publie chez `creator` un manifeste de `n` segments et rend
/// `(cid du manifeste, cids des segments)`. `present` dit, par index, si le
/// bloc existe réellement chez `creator` : un segment absent est listé au
/// manifeste avec un CID que **personne** ne détient, donc jamais récupérable
/// (`get` échoue et retente toutes les `RETRY_DELAY`) — c'est ce qui permet
/// de garantir qu'une session ne se complètera JAMAIS.
async fn publish_with(
    creator: &Node,
    tag: &str,
    present: &[bool],
) -> (champinium_core::Cid, Vec<champinium_core::Cid>) {
    let mut cids = vec![];
    for (i, p) in present.iter().enumerate() {
        let payload = format!("segment {i} de {tag} (test seed regarde)");
        cids.push(if *p {
            creator.add(payload.as_bytes()).await.unwrap()
        } else {
            champinium_core::content::cid_for(format!("absent {i} de {tag}").as_bytes())
        });
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

/// [`publish_with`] où tous les segments existent.
async fn publish(
    creator: &Node,
    tag: &str,
    n: usize,
) -> (champinium_core::Cid, Vec<champinium_core::Cid>) {
    publish_with(creator, tag, &vec![true; n]).await
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

    // Un tiers voit deux fournisseurs pour la racine (A + B). Vérification
    // complémentaire, pas la preuve du lot : sous `StorePolicy::Seed` le
    // manifeste est mis en cache ET annoncé dès `open_stream`, donc cette
    // assertion tient dès le choix de politique. Ce que le seed de ce qui est
    // regardé ajoute — la publication RETENUE et comptabilisée — est prouvé
    // par `storage_stats` et `seed_coverage` ci-dessus.
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
///
/// **Déterminisme** : le dernier segment n'existe chez PERSONNE (le manifeste
/// le liste, sa récupération échoue puis retente toutes les `RETRY_DELAY`),
/// donc la session ne peut structurellement pas se compléter. Sans ça, douze
/// blocs minuscules sur une boucle TCP locale peuvent tous arriver entre deux
/// sondages et faire échouer le test par intermittence.
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
    // Trois segments réels, un quatrième introuvable partout : la session se
    // stabilise à 3/4 et ne se complète jamais.
    let (m, cids) = publish_with(&a, "partiel", &[true, true, true, false]).await;

    let b = node(dir.path(), "b3").await;
    wire(&b, &a, addr_a).await;
    b.set_seed_watched(true).unwrap();
    seed_catalog(&b, &kp_a, m).await;

    let info = open_until_ok(&b, m).await;
    // Attend que les trois segments récupérables soient là — c'est le pire
    // cas pour la purge (le maximum de blocs à retirer), et la session reste
    // structurellement incomplète.
    poll_until(
        Duration::from_secs(60),
        "les trois segments récupérables doivent arriver",
        || {
            b.stream_status(info.id)
                .map(|s| s.fetched_segments == 3 && s.total_segments == 4)
                .unwrap_or(false)
        },
    )
    .await;
    b.close_stream(info.id).await;

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

/// Éviction à deux étages, câblage de bout en bout (spec persistance §5) : le
/// quota est saturé par une publication **regardée** (hors abonnement) ;
/// l'arrivée d'une publication d'un channel **souscrit** doit la faire partir
/// pour lui faire de la place. C'est ce qui vérifie que `make_room_for` passe
/// bien l'ensemble des émetteurs souscrits à `eviction_order` — l'unitaire
/// `eviction_prefers_unsubscribed_publications` ne teste que la fonction pure.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn watched_publication_is_evicted_to_make_room_for_a_subscription() {
    let dir = tempfile::tempdir().unwrap();

    // Y : créateur JAMAIS souscrit, dont B regarde une publication.
    let kp_y = Keypair::generate_ed25519();
    let y = Node::with_moderation(
        kp_y.clone(),
        Blockstore::open(dir.path().join("y4")).unwrap(),
        Moderation::empty(),
    )
    .await
    .unwrap();
    let addr_y = y
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    let (m_watched, segs_watched) = publish(&y, "regarde", 4).await;

    // A : créateur auquel B s'abonnera ensuite. Publication plus petite (un
    // seul segment) : une fois la place faite, elle doit tenir sous le quota.
    let kp_a = Keypair::generate_ed25519();
    let a = Node::with_moderation(
        kp_a.clone(),
        Blockstore::open(dir.path().join("a4")).unwrap(),
        Moderation::empty(),
    )
    .await
    .unwrap();
    let addr_a = a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    let (m_sub, seg_sub) = publish(&a, "abonnement", 1).await;

    let b = fast_node(dir.path(), "b4").await;
    b.listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    wire(&b, &y, addr_y).await;
    wire(&b, &a, addr_a).await;

    // 1. B regarde la publication de Y jusqu'au bout → elle entre à l'index.
    b.set_seed_watched(true).unwrap();
    seed_catalog(&b, &kp_y, m_watched).await;
    let info = open_until_ok(&b, m_watched).await;
    poll_until(
        Duration::from_secs(60),
        "la session regardée doit se compléter",
        || {
            b.stream_status(info.id)
                .map(|s| s.fetched_segments == s.total_segments)
                .unwrap_or(false)
        },
    )
    .await;
    poll_until(
        Duration::from_secs(30),
        "la publication regardée doit être indexée",
        || b.storage_stats().0 > 0,
    )
    .await;
    b.close_stream(info.id).await;
    let used_by_watched = b.storage_stats().0;

    // 2. Le quota est saturé pile par elle : plus rien ne rentre sans éviction.
    b.set_seed_quota(used_by_watched).unwrap();

    // La victime doit être STRICTEMENT mieux répliquée que le candidat
    // (dampening anti-oscillation de `make_room_for`) : Y + B = 2 pour la
    // regardée, A seul = 1 pour celle de l'abonnement. On attend la
    // convergence Kademlia avant de déclencher, sinon l'éviction serait
    // refusée sur une mesure prématurée.
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if b.replication_factor(m_watched).await.unwrap_or(0) >= 2 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("B doit être vu comme second fournisseur de la publication regardée");

    // 3. B s'abonne à A : le seed proactif doit évincer la regardée.
    b.subscribe(a.peer_id()).unwrap();
    seed_catalog(&b, &kp_a, m_sub).await;

    poll_until(
        Duration::from_secs(60),
        "la publication de l'abonnement doit être seedée à la place de la regardée",
        || {
            // Anti-flake : `make_room_for` remesure la réplication de la
            // victime et peut lire `1` de façon transitoire, ce qui refuse
            // l'éviction (inégalité stricte) et inscrit le candidat dans
            // `quota_blocked` — vidé seulement sur un évènement catalogue ou
            // quota, tous deux déjà passés ici. Réécrire le quota à sa valeur
            // courante réémet `seed_wake`, donc vide `quota_blocked` : le
            // candidat est retenté au tour suivant au lieu d'être condamné.
            let _ = b.set_seed_quota(used_by_watched);
            b.blockstore().has(&m_sub) && b.blockstore().has(&seg_sub[0])
        },
    )
    .await;
    assert!(
        !b.blockstore().has(&m_watched),
        "la publication regardée (non souscrite) doit être évincée la première"
    );
    assert!(
        segs_watched.iter().all(|c| !b.blockstore().has(c)),
        "ses segments doivent partir avec elle"
    );
    let (used, quota) = b.storage_stats();
    assert!(used <= quota, "le quota ne doit jamais être dépassé");
}

/// Quota trop petit pour la publication regardée et **rien d'évinçable** :
/// l'indexation est refusée, et AUCUN bloc ne doit rester au magasin.
///
/// Le cas piégeux est le refus **en cours de boucle** : `seed_publication`
/// abandonne au segment qui franchit le quota et retire ce qu'elle avait
/// engagé, **manifeste compris**. Les segments suivants, récupérés par la
/// session et jamais touchés par ce rollback, ne sont retrouvables que par
/// une liste capturée AVANT la tentative — sinon ils restent au magasin pour
/// toujours, hors index, hors quota et jamais réannoncés.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn watched_publication_refused_by_quota_leaves_no_orphans() {
    let dir = tempfile::tempdir().unwrap();
    let kp_a = Keypair::generate_ed25519();
    let a = Node::with_moderation(
        kp_a.clone(),
        Blockstore::open(dir.path().join("a5")).unwrap(),
        Moderation::empty(),
    )
    .await
    .unwrap();
    let addr_a = a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    let (m, cids) = publish(&a, "refuse", 3).await;
    let manifest_size = a.blockstore().size_of(&m).unwrap();

    let b = fast_node(dir.path(), "b5").await;
    wire(&b, &a, addr_a).await;
    b.set_seed_watched(true).unwrap();
    // Quota calibré pour franchir la limite AU MILIEU de la boucle : le
    // manifeste passe, le premier segment aussi, le second non. L'index est
    // vide, donc rien n'est évinçable pour financer la suite.
    b.set_seed_quota(manifest_size + 1).unwrap();
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
    // Pas d'assertion « les blocs sont d'abord là » : la boucle de seed est
    // rapide (intervalle court) et peut avoir déjà purgé au moment où on
    // regarde. Que la session `Seed` mette bien ses segments au magasin est
    // établi par `watched_stream_enters_seed_index_when_enabled` ; ici ce qui
    // compte est qu'il n'en reste RIEN, et sans la capture des segments avant
    // la tentative, ceux d'après le refus resteraient indéfiniment (le
    // sondage ci-dessous partirait en timeout).
    poll_until(
        Duration::from_secs(30),
        "aucun bloc ne doit survivre au refus de quota",
        || !b.blockstore().has(&m) && cids.iter().all(|c| !b.blockstore().has(c)),
    )
    .await;
    assert_eq!(
        b.storage_stats().0,
        0,
        "une publication refusée ne compte rien au quota"
    );
    b.close_stream(info.id).await;
    assert!(
        !b.blockstore().has(&m) && cids.iter().all(|c| !b.blockstore().has(c)),
        "la fermeture ne doit rien ressusciter"
    );
}
