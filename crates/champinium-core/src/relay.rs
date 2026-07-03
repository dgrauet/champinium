//! Nœud relais STATELESS (circuit relay v2).
//!
//! Fournit le service de relais (réservations + circuits) pour la traversée de NAT,
//! plus identify/ping. Aucun état de contenu : il ne fait que mettre en relation.
//! Les nœuds clients (voir [`crate::p2p`]) embarquent le client de relais + DCUtR
//! pour écouter/dialer via un relais et tenter un hole punching direct.

use crate::error::{CoreError, Result as CoreResult};
use crate::identity;
use futures::StreamExt;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{identify, identity::Keypair, noise, ping, relay, tcp, yamux};
use libp2p::{Multiaddr, PeerId, Swarm};
use std::time::Duration;
use tokio::sync::oneshot;

const IDENTIFY_PROTOCOL: &str = "/champinium/0.1.0";

#[derive(NetworkBehaviour)]
struct RelayBehaviour {
    relay: relay::Behaviour,
    identify: identify::Behaviour,
    ping: ping::Behaviour,
}

/// Poignée vers un relais en fonctionnement. La tâche de fond est arrêtée
/// automatiquement quand la poignée est abandonnée (`Drop` abort le `JoinHandle`)
/// : indispensable pour les tests, qui sinon fuiraient une tâche par relais.
pub struct RelayHandle {
    /// PeerId du relais.
    pub peer_id: PeerId,
    /// Adresse d'écoute effective (à référencer dans une adresse de circuit).
    pub addr: Multiaddr,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for RelayHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl RelayHandle {
    /// Adresse de circuit à utiliser par les clients : `<addr>/p2p/<peer>/p2p-circuit`.
    pub fn circuit_addr(&self) -> CoreResult<Multiaddr> {
        format!("{}/p2p/{}/p2p-circuit", self.addr, self.peer_id)
            .parse()
            .map_err(|e| CoreError::Network(format!("adresse de circuit invalide: {e}")))
    }
}

/// Démarre un relais : écoute sur `listen`, tourne dans une tâche tokio dédiée,
/// et renvoie sa poignée une fois l'adresse d'écoute connue.
pub async fn start_relay(keypair: Keypair, listen: Multiaddr) -> CoreResult<RelayHandle> {
    let peer_id = identity::peer_id(&keypair);
    let mut swarm = build_relay_swarm(keypair)?;
    swarm
        .listen_on(listen)
        .map_err(|e| CoreError::Network(e.to_string()))?;

    let (tx, rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let mut announce = Some(tx);
        loop {
            if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
                // Confirme l'adresse comme externe : sans cela, le relais accepte
                // les réservations mais ne fournit aucune adresse, et les clients
                // ne peuvent pas construire/annoncer leur adresse relayée.
                swarm.add_external_address(address.clone());
                if let Some(tx) = announce.take() {
                    let _ = tx.send(address);
                }
            }
        }
    });

    let addr = rx.await.map_err(|_| CoreError::Shutdown)?;
    Ok(RelayHandle {
        peer_id,
        addr,
        task,
    })
}

fn build_relay_swarm(keypair: Keypair) -> CoreResult<Swarm<RelayBehaviour>> {
    let swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .map_err(|e| CoreError::Network(e.to_string()))?
        .with_behaviour(|key| {
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(RelayBehaviour {
                relay: relay::Behaviour::new(key.public().to_peer_id(), relay::Config::default()),
                identify: identify::Behaviour::new(identify::Config::new(
                    IDENTIFY_PROTOCOL.to_string(),
                    key.public(),
                )),
                ping: ping::Behaviour::default(),
            })
        })
        .map_err(|e| CoreError::Network(e.to_string()))?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();
    Ok(swarm)
}
