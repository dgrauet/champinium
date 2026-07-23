//! Tests d'intégration du module `coldstore` (lot CS-a, Tâche 2).
//!
//! Aucun réseau réel : les gateways Arweave sont simulées par `wiremock`. Le
//! test (c) est le garde-fou anti-gateway-malveillante : une gateway qui sert
//! des octets ne correspondant pas au CID demandé doit être rejetée, jamais
//! utilisée telle quelle.
#![cfg(feature = "cold-storage")]

use champinium_core::coldstore::arweave::ArweaveColdStore;
use champinium_core::coldstore::receipts::{load_receipts, save_receipt};
use champinium_core::coldstore::{ArchiveReceipt, ArweaveWallet, ColdStore};
use champinium_core::content::cid_for;
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

#[test]
fn archive_receipt_roundtrips_json() {
    let receipt = ArchiveReceipt {
        manifest_cid: "bafy-manifeste".to_string(),
        tx_id: "tx-abc".to_string(),
        timestamp: 1_753_000_000,
        bytes: 123_456,
        cost_winston: 987_654_321,
    };
    let json = serde_json::to_string(&receipt).unwrap();
    let restored: ArchiveReceipt = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.manifest_cid, receipt.manifest_cid);
    assert_eq!(restored.tx_id, receipt.tx_id);
    assert_eq!(restored.timestamp, receipt.timestamp);
    assert_eq!(restored.bytes, receipt.bytes);
    assert_eq!(restored.cost_winston, receipt.cost_winston);
}

#[test]
fn receipts_roundtrip_via_dotfile() {
    let dir = tempfile::tempdir().unwrap();
    let receipt = ArchiveReceipt {
        manifest_cid: "bafy-x".to_string(),
        tx_id: "tx-x".to_string(),
        timestamp: 42,
        bytes: 10,
        cost_winston: 5,
    };
    save_receipt(dir.path(), &receipt).unwrap();
    let loaded = load_receipts(dir.path());
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].tx_id, "tx-x");
}

#[test]
fn receipts_load_defaults_when_absent() {
    let dir = tempfile::tempdir().unwrap();
    let loaded = load_receipts(dir.path());
    assert!(loaded.is_empty());
}

#[test]
fn receipts_load_tolerates_corruption() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".archives"), b"{not valid json").unwrap();
    let loaded = load_receipts(dir.path());
    assert!(loaded.is_empty());
}

#[test]
fn wallet_from_path_rejects_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no-such-wallet.json");
    let result = ArweaveWallet::from_path(&missing);
    assert!(result.is_err());
}

#[cfg(unix)]
#[test]
fn wallet_from_path_rejects_overly_open_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let wallet_path = dir.path().join("wallet.json");
    std::fs::write(&wallet_path, b"{}").unwrap();
    std::fs::set_permissions(&wallet_path, std::fs::Permissions::from_mode(0o644)).unwrap();

    let result = ArweaveWallet::from_path(&wallet_path);
    assert!(result.is_err());
}

#[cfg(unix)]
#[test]
fn wallet_from_path_accepts_correct_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let wallet_path = dir.path().join("wallet.json");
    std::fs::write(&wallet_path, b"{}").unwrap();
    std::fs::set_permissions(&wallet_path, std::fs::Permissions::from_mode(0o600)).unwrap();

    let result = ArweaveWallet::from_path(&wallet_path);
    assert!(result.is_ok());
}
