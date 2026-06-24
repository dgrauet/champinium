//! Test d'intégration Phase 4 : transfert de bloc via un circuit relay v2.
//!
//! Trois nœuds en loopback : un **relais** R, un **listener** A qui n'écoute QUE
//! via le relais (adresse de circuit), et un **dialer** B qui atteint A *via le
//! relais* et récupère un bloc. Prouve la traversée par circuit relay.

use champinium_core::identity::load_or_generate;
use champinium_core::{start_relay, Blockstore, Moderation, Node};
use std::time::Duration;

async fn node(dir: &std::path::Path, name: &str) -> Node {
    let kp = load_or_generate(dir.join(format!("{name}.key"))).unwrap();
    let bs = Blockstore::open(dir.join(name)).unwrap();
    Node::with_moderation(kp, bs, Moderation::empty())
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn block_transfer_over_relay_circuit() {
    let dir = tempfile::tempdir().unwrap();

    // Relais.
    let relay_kp = load_or_generate(dir.path().join("relay.key")).unwrap();
    let relay = start_relay(relay_kp, "/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    let circuit = relay.circuit_addr().unwrap();
    let relay_dial = format!("{}/p2p/{}", relay.addr, relay.peer_id)
        .parse()
        .unwrap();

    // A : enregistre D'ABORD l'écoute via circuit (crée le listener relais), PUIS
    // se connecte au relais — la connexion établie déclenche la réservation.
    let node_a = node(dir.path(), "a").await;
    node_a
        .add_address(relay.peer_id, relay.addr.clone())
        .await
        .unwrap();
    let listen_task = {
        let na = node_a.clone();
        tokio::spawn(async move { na.listen(circuit).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    node_a.dial(relay_dial).await.unwrap();

    // Adresse relayée annoncée par A (se termine par /p2p/<A>, directement diallable).
    let a_relayed = tokio::time::timeout(Duration::from_secs(20), listen_task)
        .await
        .expect("la réservation de relais doit aboutir")
        .expect("tâche listen")
        .unwrap();
    let cid = node_a.add(b"contenu transfere via relais").await.unwrap();

    // B : atteint A *via le relais* et récupère le bloc.
    let node_b = node(dir.path(), "b").await;
    let received = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let _ = node_b.dial(a_relayed.clone()).await;
            if let Ok(bytes) = node_b.request_block(node_a.peer_id(), cid).await {
                break bytes;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .expect("transfert via circuit relay dans le délai imparti");

    assert_eq!(received, b"contenu transfere via relais");
}
