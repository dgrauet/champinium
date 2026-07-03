//! Test d'intégration : notification « catalogue mis à jour ».
//!
//! Preuve : un abonné (`Node::subscribe_catalog`) est réveillé quand le
//! catalogue local change — que le feed arrive par gossipsub (pair distant) ou
//! par une publication locale. C'est le socle du flux d'événements FFI qui
//! remplace le délai codé en dur des fronts.

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscriber_is_notified_when_gossip_feed_updates_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let node_a = node(dir.path(), "a").await;
    let node_b = node(dir.path(), "b").await;

    let addr_a = node_a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    node_b
        .add_address(node_a.peer_id(), addr_a.clone())
        .await
        .unwrap();
    node_b.dial(addr_a).await.unwrap();

    let c1 = node_a.add(b"contenu notifie").await.unwrap();
    let mut events = node_b.subscribe_catalog();

    // A republie tant que le mesh gossipsub n'est pas formé ; B doit être
    // réveillé par la notification (pas par un poll du catalogue).
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            node_a.publish_feed(&[c1]).await.unwrap();
            match tokio::time::timeout(Duration::from_millis(500), events.recv()).await {
                Ok(Ok(())) => break,
                _ => continue,
            }
        }
    })
    .await
    .expect("l'abonné doit être notifié de la mise à jour du catalogue");

    // La notification correspond bien à un catalogue peuplé.
    assert!(node_b.catalog_cids().contains(&c1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscriber_is_notified_on_local_publish() {
    let dir = tempfile::tempdir().unwrap();
    let node_a = node(dir.path(), "solo").await;

    let c1 = node_a.add(b"contenu local").await.unwrap();
    let mut events = node_a.subscribe_catalog();

    node_a.publish_feed(&[c1]).await.unwrap();

    tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("notification attendue après publication locale")
        .expect("canal d'événements vivant");
}
