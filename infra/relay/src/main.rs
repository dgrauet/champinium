//! champinium-relay — relais NAT (circuit relay v2 + DCUtR), Phase 4.
//!
//! SANS ÉTAT : fournit le service de relais (réservations + circuits) pour les
//! nœuds derrière NAT, et assiste le hole punching (DCUtR côté clients). Ne stocke
//! aucun contenu ; ne persiste que sa clé d'identité (PeerId/multiaddr stable).
//! N'importe qui peut lancer le sien (voir docs/).

use std::path::PathBuf;

use anyhow::{Context, Result};
use champinium_core::identity::load_or_generate;
use champinium_core::start_relay;
use clap::Parser;

#[derive(Parser)]
#[command(
    name = "champinium-relay",
    about = "Relais NAT stateless (circuit relay v2)"
)]
struct Cli {
    /// Adresse d'écoute.
    #[arg(long, default_value = "/ip4/0.0.0.0/tcp/4201")]
    listen: String,
    /// Répertoire pour la clé d'identité (PeerId stable).
    #[arg(long, default_value = ".champinium-relay")]
    data_dir: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "champinium_core=info".into()),
        )
        .init();

    let cli = Cli::parse();
    let keypair = load_or_generate(cli.data_dir.join("node.key"))?;
    let handle = start_relay(
        keypair,
        cli.listen.parse().context("multiaddr d'écoute invalide")?,
    )
    .await?;

    println!("champinium-relay en ligne (stateless)");
    println!("PeerId : {}", handle.peer_id);
    println!("Adresse: {}/p2p/{}", handle.addr, handle.peer_id);
    println!("Circuit : {}", handle.circuit_addr()?);
    println!("Nœuds NAT : écoutez sur <circuit>. Autres : dialez <circuit>/p2p/<peer-NAT>.");

    tokio::signal::ctrl_c().await?;
    Ok(())
}
