//! champinium-bootstrap — nœud de rendez-vous initial (Phase 1).
//!
//! SANS ÉTAT au sens « contenu » : il ne stocke aucun bloc nécessaire au réseau.
//! Son rôle est d'être un point de rendez-vous Kademlia stable pour la découverte
//! de pairs. Sa clé d'identité est persistée pour offrir un PeerId/multiaddr
//! stable (que des tiers peuvent référencer) — c'est de la configuration, pas de
//! l'état réseau. N'importe qui peut lancer le sien (voir docs/).

use std::path::PathBuf;

use anyhow::{Context, Result};
use champinium_core::identity::load_or_generate;
use champinium_core::{Blockstore, Node};
use clap::Parser;

#[derive(Parser)]
#[command(name = "champinium-bootstrap", about = "Nœud bootstrap stateless")]
struct Cli {
    /// Adresse d'écoute.
    #[arg(long, default_value = "/ip4/0.0.0.0/tcp/4101")]
    listen: String,
    /// Répertoire pour la clé d'identité (PeerId stable).
    #[arg(long, default_value = ".champinium-bootstrap")]
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
    // Magasin de blocs éphémère : un bootstrap ne sert pas de contenu.
    let blockstore = Blockstore::open(std::env::temp_dir().join("champinium-bootstrap-blocks"))?;
    let node = Node::new(keypair, blockstore).await?;

    let addr = node
        .listen(cli.listen.parse().context("multiaddr d'écoute invalide")?)
        .await?;

    println!("champinium-bootstrap en ligne (stateless)");
    println!("PeerId : {}", node.peer_id());
    println!("Adresse: {addr}/p2p/{}", node.peer_id());
    println!("Référez ce multiaddr comme --bootstrap chez les autres nœuds.");

    tokio::signal::ctrl_c().await?;
    Ok(())
}
