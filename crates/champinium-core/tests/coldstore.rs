//! Tests d'intégration du module `coldstore` (lot CS-a) — **récupération/repli
//! uniquement** (l'archivage Arweave est différé, voir `coldstore`).
//!
//! Aucun réseau réel : les gateways Arweave sont simulées par `wiremock`. Le
//! test (c) est le garde-fou anti-gateway-malveillante : une gateway qui sert
//! des octets ne correspondant pas au CID demandé doit être rejetée, jamais
//! utilisée telle quelle.
#![cfg(feature = "cold-storage")]

use champinium_core::coldstore::arweave::ArweaveColdStore;
use champinium_core::coldstore::ColdStore;
use champinium_core::content::cid_for;
use champinium_core::identity::load_or_generate;
use champinium_core::{Blockstore, Cid, CoreError, Denylist, Moderation, Node};
use std::sync::Arc;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Corps JSON GraphQL renvoyant une seule transaction `tx_id` pour le tag
/// `champinium-cid`.
fn graphql_hit_body(tx_id: &str) -> serde_json::Value {
    serde_json::json!({
        "data": {
            "transactions": {
                "edges": [
                    { "node": { "id": tx_id } }
                ]
            }
        }
    })
}

/// Corps GraphQL sans résultat (aucune transaction taguée pour ce CID).
fn graphql_miss_body() -> serde_json::Value {
    serde_json::json!({
        "data": { "transactions": { "edges": [] } }
    })
}

#[tokio::test]
async fn retrieve_returns_bytes_when_cid_matches() {
    let server = MockServer::start().await;
    let bytes = b"contenu archive valide";
    let cid = cid_for(bytes);
    let tx_id = "tx-valide-1";

    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(graphql_hit_body(tx_id)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/{tx_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.to_vec()))
        .mount(&server)
        .await;

    let store = ArweaveColdStore::new(vec![server.uri()]);
    let result = store.retrieve(cid).await.unwrap();

    assert_eq!(result, Some(bytes.to_vec()));
}

#[tokio::test]
async fn retrieve_returns_none_when_all_gateways_miss() {
    let server_a = MockServer::start().await;
    let server_b = MockServer::start().await;
    let bytes = b"contenu introuvable";
    let cid = cid_for(bytes);

    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server_a)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(graphql_miss_body()))
        .mount(&server_b)
        .await;

    let store = ArweaveColdStore::new(vec![server_a.uri(), server_b.uri()]);
    let result = store.retrieve(cid).await.unwrap();

    assert_eq!(result, None);
}

#[tokio::test]
async fn retrieve_rejects_cid_mismatch_and_tries_next_gateway() {
    let malicious = MockServer::start().await;
    let honest = MockServer::start().await;
    let bytes = b"contenu authentique";
    let cid = cid_for(bytes);
    let tampered = b"contenu falsifie par la gateway malveillante";
    let tx_id = "tx-2";

    // La gateway malveillante prétend avoir une transaction pour ce CID, mais
    // sert des octets différents (falsifiés).
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(graphql_hit_body(tx_id)))
        .mount(&malicious)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/{tx_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(tampered.to_vec()))
        .mount(&malicious)
        .await;

    // La gateway honnête sert le bon contenu.
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(graphql_hit_body(tx_id)))
        .mount(&honest)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/{tx_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.to_vec()))
        .mount(&honest)
        .await;

    let store = ArweaveColdStore::new(vec![malicious.uri(), honest.uri()]);
    let result = store.retrieve(cid).await.unwrap();

    // Les octets falsifiés ne sont JAMAIS renvoyés : soit on obtient le bon
    // contenu via la gateway suivante, soit None — jamais le contenu tamponné.
    assert_eq!(result, Some(bytes.to_vec()));
}

#[tokio::test]
async fn retrieve_returns_none_when_only_malicious_gateway_available() {
    let malicious = MockServer::start().await;
    let bytes = b"contenu authentique";
    let cid = cid_for(bytes);
    let tampered = b"faux contenu";
    let tx_id = "tx-3";

    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(graphql_hit_body(tx_id)))
        .mount(&malicious)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/{tx_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(tampered.to_vec()))
        .mount(&malicious)
        .await;

    let store = ArweaveColdStore::new(vec![malicious.uri()]);
    let result = store.retrieve(cid).await.unwrap();

    // Jamais le contenu falsifié : au pire, silence (None).
    assert_eq!(result, None);
}

#[tokio::test]
async fn retrieve_rejects_oversized_gateway_response_and_tries_next_gateway() {
    let malicious = MockServer::start().await;
    let honest = MockServer::start().await;
    let bytes = b"contenu authentique";
    let cid = cid_for(bytes);
    let tx_id = "tx-oversized";
    // Un peu plus que la borne MAX_ARCHIVE_FETCH_BYTES (64 MiO) d'arweave.rs :
    // une gateway (même malveillante) ne doit jamais pouvoir faire gonfler la
    // mémoire du client par une réponse énorme.
    let oversized = vec![0u8; 64 * 1024 * 1024 + 1];

    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(graphql_hit_body(tx_id)))
        .mount(&malicious)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/{tx_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(oversized))
        .mount(&malicious)
        .await;

    // La gateway suivante, honnête, sert le bon contenu (taille normale).
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(graphql_hit_body(tx_id)))
        .mount(&honest)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/{tx_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.to_vec()))
        .mount(&honest)
        .await;

    let store = ArweaveColdStore::new(vec![malicious.uri(), honest.uri()]);
    let result = store.retrieve(cid).await.unwrap();

    assert_eq!(result, Some(bytes.to_vec()));
}

// --- Repli de récupération froid dans `Node::get_with`. ---
//
// `Node::get_with` (politique de stockage) et `StorePolicy` sont internes au
// crate (`pub(crate)`) : les tests ci-dessous passent par les passerelles
// `#[doc(hidden)]` `Node::with_cold_for_tests` / `Node::get_with_policy_for_tests`
// — aucune autre API publique n'est élargie.

/// Sert un unique CID connu depuis un backend froid simulé (pas de réseau
/// réel : juste une paire (CID, octets) en mémoire).
struct MockColdStore {
    cid: Cid,
    bytes: Vec<u8>,
}

#[async_trait::async_trait]
impl ColdStore for MockColdStore {
    async fn retrieve(&self, cid: Cid) -> champinium_core::Result<Option<Vec<u8>>> {
        Ok(if cid == self.cid {
            Some(self.bytes.clone())
        } else {
            None
        })
    }
}

async fn solo_node(dir: &std::path::Path, name: &str) -> Node {
    let kp = load_or_generate(dir.join(format!("{name}.key"))).unwrap();
    let bs = Blockstore::open(dir.join(name)).unwrap();
    Node::with_moderation(kp, bs, Moderation::empty())
        .await
        .unwrap()
}

/// (1) Aucun fournisseur P2P (nœud isolé) : le repli froid sert quand même
/// les octets.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fallback_serves_bytes_when_no_p2p_provider() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = b"contenu uniquement au froid, aucun pair ne l'a".to_vec();
    let cid = cid_for(&bytes);

    let node = solo_node(dir.path(), "solo").await;
    let cold = Arc::new(MockColdStore {
        cid,
        bytes: bytes.clone(),
    });
    let node = node.with_cold_for_tests(cold);

    let got = node
        .get_with_policy_for_tests(cid, false)
        .await
        .expect("le repli froid doit servir le contenu");
    assert_eq!(got, bytes);
}

/// (2) Sous politique Seed, le bloc récupéré au froid entre au blockstore ET
/// le nœud devient fournisseur (réamorce le P2P) — vérifié depuis un second
/// nœud qui se connecte APRÈS coup, comme `reprovide_makes_stored_blocks_discoverable`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fallback_under_seed_policy_enters_blockstore_and_reprovides() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = b"contenu froid a reamorcer sur le p2p".to_vec();
    let cid = cid_for(&bytes);

    let node_b = solo_node(dir.path(), "b").await;
    let cold = Arc::new(MockColdStore {
        cid,
        bytes: bytes.clone(),
    });
    let node_b = node_b.with_cold_for_tests(cold);

    let got = node_b
        .get_with_policy_for_tests(cid, true)
        .await
        .expect("le repli froid doit servir le contenu sous Seed");
    assert_eq!(got, bytes);
    assert!(
        node_b.blockstore().has(&cid),
        "Seed doit mettre le bloc récupéré au froid en cache"
    );

    // B réannonce déjà côté DHT locale ; A se connecte ensuite et doit le
    // découvrir comme fournisseur.
    let node_a = solo_node(dir.path(), "a").await;
    let addr_b = node_b
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    node_a
        .add_address(node_b.peer_id(), addr_b.clone())
        .await
        .unwrap();
    node_a.dial(addr_b).await.unwrap();

    let found = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let providers = node_a.get_providers(cid).await.unwrap_or_default();
            if providers.contains(&node_b.peer_id()) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    })
    .await
    .expect("B doit devenir fournisseur découvrable après le repli Seed");
    assert!(found);
}

/// (3) Débrayé (`set_cold_retrieval(false)`) : pas de repli, `NoProviders`
/// propagée telle quelle malgré un `ColdStore` capable de servir le contenu.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fallback_disabled_propagates_no_providers() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = b"contenu froid disponible mais repli debraye".to_vec();
    let cid = cid_for(&bytes);

    let node = solo_node(dir.path(), "solo").await;
    let cold = Arc::new(MockColdStore {
        cid,
        bytes: bytes.clone(),
    });
    let node = node.with_cold_for_tests(cold);
    assert!(node.cold_retrieval_enabled(), "activé par défaut");
    node.set_cold_retrieval(false).unwrap();
    assert!(!node.cold_retrieval_enabled());

    let err = node
        .get_with_policy_for_tests(cid, false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, CoreError::NoProviders(_)),
        "débrayé, aucun repli : NoProviders doit être propagée telle quelle"
    );
}

/// (4) Un contenu en denylist récupéré depuis le froid reste refusé —
/// checkpoint modération #2 inchangé, qu'il vienne du P2P ou du froid.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fallback_still_enforces_moderation_checkpoint_two() {
    let dir = tempfile::tempdir().unwrap();
    let forbidden = b"contenu interdit, meme recupere au froid".to_vec();
    let cid = cid_for(&forbidden);

    let issuer = load_or_generate(dir.path().join("issuer.key")).unwrap();
    let dl = Denylist::build_signed("test", "2026-07-23T00:00:00Z", &issuer, &[cid], &[]).unwrap();
    let mut moderation = Moderation::empty();
    moderation.subscribe(&dl).unwrap();

    let kp = load_or_generate(dir.path().join("node.key")).unwrap();
    let bs = Blockstore::open(dir.path().join("blocks")).unwrap();
    let node = Node::with_moderation(kp, bs, moderation).await.unwrap();
    let cold = Arc::new(MockColdStore {
        cid,
        bytes: forbidden.clone(),
    });
    let node = node.with_cold_for_tests(cold);

    let err = node
        .get_with_policy_for_tests(cid, false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, CoreError::Moderated(_)),
        "checkpoint #2 doit refuser le contenu même récupéré au froid"
    );
    assert!(
        !node.blockstore().has(&cid),
        "un contenu refusé ne doit jamais entrer au blockstore"
    );
}

/// (5) Défense en profondeur : un backend froid menteur (les octets rendus
/// ne correspondent pas au CID demandé) ne doit JAMAIS voir ses octets
/// renvoyés — même sous `Stream`, qui ne repasse pas par `blockstore.put`
/// (content-addressed) pour une vérification gratuite. Traité comme un
/// fournisseur absent (`NoProviders`), rien n'entre au blockstore.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fallback_rejects_cid_mismatch_from_cold_store() {
    let dir = tempfile::tempdir().unwrap();
    let requested = b"contenu authentique demande par CID".to_vec();
    let cid = cid_for(&requested);
    let tampered = b"octets differents rendus par un cold store menteur ou bogue".to_vec();

    let node = solo_node(dir.path(), "solo").await;
    let cold = Arc::new(MockColdStore {
        cid,
        bytes: tampered,
    });
    let node = node.with_cold_for_tests(cold);

    let err = node
        .get_with_policy_for_tests(cid, false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, CoreError::NoProviders(_)),
        "un CID ne correspondant pas doit être traité comme un fournisseur absent, jamais servi"
    );
    assert!(
        !node.blockstore().has(&cid),
        "des octets non vérifiés ne doivent jamais entrer au blockstore"
    );
}
