//! Tests d'intégration : recherche décentralisée (Phase 5, issue #20).
//!
//! Deux étages, aux limites assumées (risque #4 du spec) :
//! - **index local** : `Node::search` interroge le catalogue reconstruit par
//!   écoute gossip (ne couvre que ce que le nœud a vu passer) ;
//! - **tags DHT** : l'émetteur d'un feed v2 s'annonce fournisseur de
//!   `/champinium/tag/<tag>` ; `Node::search_tag` retrouve les émetteurs via la
//!   DHT puis récupère et vérifie leurs feeds — découverte **hors gossip**.

use champinium_core::feed::FeedEntry;
use champinium_core::identity::load_or_generate;
use champinium_core::{Blockstore, Moderation, Node};
use std::time::Duration;

async fn node(dir: &std::path::Path, name: &str) -> Node {
    let kp = load_or_generate(dir.join(format!("{name}.key"))).unwrap();
    let bs = Blockstore::open(dir.join(name)).unwrap();
    Node::with_moderation(kp, bs, Moderation::empty())
        .await
        .unwrap()
}

fn entry(cid: champinium_core::Cid, title: &str, tags: &[&str]) -> FeedEntry {
    FeedEntry {
        cid: cid.to_string(),
        title: title.to_string(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
    }
}

/// Un feed v2 avec métadonnées se propage en gossip et devient cherchable
/// localement chez le pair (titre, insensible à la casse).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn published_metadata_is_searchable_on_peer() {
    let dir = tempfile::tempdir().unwrap();
    let creator = node(dir.path(), "creator").await;
    let viewer = node(dir.path(), "viewer").await;

    let addr = creator
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    viewer
        .add_address(creator.peer_id(), addr.clone())
        .await
        .unwrap();
    viewer.dial(addr).await.unwrap();

    let cid = creator.add(b"les aurores").await.unwrap();
    let meta = entry(cid, "Aurores boréales", &["nature"]);

    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            creator
                .publish_feed_with(std::slice::from_ref(&meta))
                .await
                .unwrap();
            let hits = viewer.search("aurores");
            if !hits.is_empty() {
                assert_eq!(hits[0].cid, cid);
                assert_eq!(hits[0].issuer, creator.peer_id());
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .expect("le titre publié doit devenir cherchable chez le pair");
}

/// Découverte par tag HORS gossip : un chercheur qui n'a jamais reçu le feed
/// retrouve le contenu via les fournisseurs DHT du tag, puis le feed signé.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_tag_discovers_content_via_dht() {
    let dir = tempfile::tempdir().unwrap();
    let creator = node(dir.path(), "tag-creator").await;
    let searcher = node(dir.path(), "tag-searcher").await;

    let addr = creator
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    searcher
        .add_address(creator.peer_id(), addr.clone())
        .await
        .unwrap();
    searcher.dial(addr).await.unwrap();

    let cid = creator.add(b"documentaire foret").await.unwrap();
    let meta = entry(cid, "Forêt primaire", &["Nature"]); // normalisé → "nature"

    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            creator
                .publish_feed_with(std::slice::from_ref(&meta))
                .await
                .unwrap();
            let hits = searcher.search_tag("nature").await.unwrap();
            if !hits.is_empty() {
                assert_eq!(hits[0].cid, cid);
                assert_eq!(hits[0].title, "Forêt primaire");
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .expect("le tag doit être découvrable via la DHT");
}
