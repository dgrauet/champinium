//! Tests d'intégration : signalement P2P des contenus bloqués (Phase 5).
//!
//! Quand la modération refuse un contenu au checkpoint #2 (réception), le nœud
//! émet un **rapport signé** `champinium-report/v1` sur un topic gossip dédié.
//! Les pairs vérifient la signature et agrègent (borné) le nombre de
//! rapporteurs distincts par CID — matière première des éditeurs de denylists.

use champinium_core::content::cid_for;
use champinium_core::identity::load_or_generate;
use champinium_core::moderation::Denylist;
use champinium_core::report::{Report, ReportTally};
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

/// Un rapport signé se vérifie ; altéré (CID substitué), il est rejeté.
#[test]
fn report_roundtrips_and_rejects_tampering() {
    let reporter = Keypair::generate_ed25519();
    let cid = cid_for(b"contenu interdit");
    let report = Report::build_signed(&reporter, &cid, "denylist").unwrap();

    let json = report.to_json().unwrap();
    let parsed = Report::from_json(json.as_bytes()).unwrap();
    parsed.verify().expect("signature valide");
    assert_eq!(parsed.cid().unwrap(), cid);

    let mut forged = parsed.clone();
    forged.cid = cid_for(b"autre contenu").to_string();
    assert!(forged.verify().is_err(), "un rapport altéré est rejeté");
}

/// Quand un nœud refuse un contenu au checkpoint #2 (get d'un CID bloqué par
/// une denylist souscrite), un pair observateur reçoit le rapport signé et
/// l'agrège : `report_count(cid)` passe à 1 chez lui, à l'étage « autres »
/// (l'observateur n'a pas souscrit la clé de la victime comme éditeur de
/// denylist).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocked_get_emits_report_that_peers_aggregate() {
    let dir = tempfile::tempdir().unwrap();
    let observer = node(dir.path(), "observer").await;
    let victim = node(dir.path(), "victim").await;

    let addr_o = observer
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    victim
        .add_address(observer.peer_id(), addr_o.clone())
        .await
        .unwrap();
    victim.dial(addr_o).await.unwrap();

    // La victime souscrit une denylist qui bloque `bad`.
    let issuer = Keypair::generate_ed25519();
    let bad = cid_for(b"contenu signale");
    let dl = Denylist::build_signed("t", "2026-07-04T00:00:00Z", &issuer, 1, &[bad], &[]).unwrap();
    victim.subscribe_denylist(&dl).await.unwrap();

    assert_eq!(observer.report_count(&bad), ReportTally::default());

    // Chaque tentative de récupération d'un CID bloqué est refusée ET émet un
    // rapport. On retente le temps que le mesh gossip se forme.
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            assert!(victim.get(bad).await.is_err(), "le CID bloqué est refusé");
            if observer.report_count(&bad).total() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .expect("l'observateur doit agréger le rapport de la victime");

    // Le même rapporteur ne compte qu'une fois (rapporteurs DISTINCTS), et il
    // n'est pas une clé de confiance chez l'observateur.
    assert_eq!(
        observer.report_count(&bad),
        ReportTally {
            trusted: 0,
            others: 1
        }
    );
}

/// Pondération par les clés de confiance : quand l'observateur a souscrit la
/// clé de la victime comme **éditeur de denylist**, le rapport de celle-ci
/// compte à l'étage « de confiance » — et redescend à l'étage « autres » dès
/// le désabonnement (reclassement à chaud, `ReportBook::retrust`). Souscrire
/// une clé n'exige aucune liste publiée : c'est la clé qui est suivie.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn report_from_subscribed_denylist_issuer_counts_as_trusted() {
    let dir = tempfile::tempdir().unwrap();
    let observer = node(dir.path(), "observer-trust").await;
    let victim = node(dir.path(), "victim-trust").await;

    let addr_o = observer
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    victim
        .add_address(observer.peer_id(), addr_o.clone())
        .await
        .unwrap();
    victim.dial(addr_o).await.unwrap();

    // L'observateur fait confiance à la clé de la victime (aucune liste de cet
    // éditeur n'a besoin d'exister : on souscrit la clé, pas un contenu).
    observer
        .subscribe_denylist_issuer(victim.peer_id())
        .unwrap();

    // La victime souscrit une denylist qui bloque `bad` (même mécanisme que le
    // test ci-dessus) pour que son checkpoint #2 émette un rapport signé.
    let issuer = Keypair::generate_ed25519();
    let bad = cid_for(b"contenu signale par une cle de confiance");
    let dl = Denylist::build_signed("t", "2026-09-12T00:00:00Z", &issuer, 1, &[bad], &[]).unwrap();
    victim.subscribe_denylist(&dl).await.unwrap();

    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            assert!(victim.get(bad).await.is_err(), "le CID bloqué est refusé");
            if observer.report_count(&bad).total() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .expect("l'observateur doit agréger le rapport de la victime");

    assert_eq!(
        observer.report_count(&bad),
        ReportTally {
            trusted: 1,
            others: 0
        },
        "un éditeur souscrit compte à l'étage de confiance"
    );

    // Désabonnement : la clé n'est plus de confiance, son rapport redescend.
    observer
        .unsubscribe_denylist_issuer(victim.peer_id())
        .await
        .unwrap();
    assert_eq!(
        observer.report_count(&bad),
        ReportTally {
            trusted: 0,
            others: 1
        },
        "le reclassement redescend le rapporteur"
    );
}
