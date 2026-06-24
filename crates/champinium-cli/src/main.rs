//! champinium-cli — outil de debug du noyau (Phase 1).
//!
//! Démo P2P entre deux nœuds :
//!   # nœud A : publie un fichier et reste en ligne pour le servir
//!   champinium-cli --data-dir /tmp/a add ./media.bin --listen /ip4/0.0.0.0/tcp/4001
//!   # -> affiche le CID et une adresse /ip4/.../tcp/4001/p2p/<peerA>
//!
//!   # nœud B : récupère le bloc via la DHT depuis A
//!   champinium-cli --data-dir /tmp/b get <CID> --peer /ip4/127.0.0.1/tcp/4001/p2p/<peerA> --out ./out.bin

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use champinium_core::identity::{load_or_generate, peer_id};
use champinium_core::p2p::split_peer_id;
use champinium_core::{Blockstore, Cid, Denylist, Moderation, Node};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "champinium-cli", about = "Debug CLI du noyau Champinium")]
struct Cli {
    /// Répertoire de données du nœud (clé d'identité + blocs).
    #[arg(long, default_value = ".champinium")]
    data_dir: PathBuf,
    /// Denylists signées à souscrire (JSON `champinium-denylist/v1`), répétable.
    /// La denylist par défaut reste toujours active (non désactivable).
    #[arg(long)]
    denylist: Vec<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Affiche le PeerId du nœud.
    Id,
    /// Démarre un nœud qui écoute (et se connecte à des bootstrap), puis reste en ligne.
    Serve {
        #[arg(long, default_value = "/ip4/0.0.0.0/tcp/0")]
        listen: String,
        /// Adresses de bootstrap (`/ip4/.../tcp/.../p2p/<peerid>`), répétable.
        #[arg(long)]
        bootstrap: Vec<String>,
    },
    /// Publie un fichier (stocke + annonce dans la DHT), puis reste en ligne pour le servir.
    Add {
        path: PathBuf,
        #[arg(long, default_value = "/ip4/0.0.0.0/tcp/0")]
        listen: String,
    },
    /// Récupère un bloc par CID depuis un pair.
    Get {
        cid: String,
        /// Adresse du pair `/ip4/.../tcp/.../p2p/<peerid>`.
        #[arg(long)]
        peer: String,
        /// Fichier de sortie (sinon: nombre d'octets sur stdout).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Liste les fournisseurs d'un CID via la DHT.
    FindProviders {
        cid: String,
        #[arg(long)]
        peer: String,
    },
    /// Reconstruit et affiche le catalogue en écoutant les feeds d'un pair.
    Catalog {
        /// Pair auquel se connecter `/ip4/.../tcp/.../p2p/<peerid>`.
        #[arg(long)]
        peer: String,
        /// Durée d'écoute des feeds (secondes).
        #[arg(long, default_value = "6")]
        wait: u64,
    },
    /// Récupère le feed d'un créateur depuis la DHT (hors gossip).
    FetchFeed {
        /// Pair auquel se connecter `/ip4/.../tcp/.../p2p/<peerid>`.
        #[arg(long)]
        peer: String,
        /// PeerId du créateur dont on veut le feed.
        #[arg(long)]
        issuer: String,
    },
    /// Ingère un média (ffmpeg → HLS), publie son manifeste et reste en ligne.
    Ingest {
        path: PathBuf,
        #[arg(long, default_value = "/ip4/0.0.0.0/tcp/0")]
        listen: String,
    },
    /// Reconstruit un HLS jouable depuis un manifeste, récupéré depuis un pair.
    FetchHls {
        /// CID du manifeste HLS.
        manifest: String,
        #[arg(long)]
        peer: String,
        /// Répertoire de sortie (recevra index.m3u8 + segments).
        #[arg(long)]
        out: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "champinium_core=info,champinium_cli=info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Id => {
            let kp = load_or_generate(cli.data_dir.join("node.key"))?;
            println!("{}", peer_id(&kp));
        }
        Cmd::Serve { listen, bootstrap } => {
            let node = build_node(&cli.data_dir, &cli.denylist).await?;
            let addr = node
                .listen(listen.parse().context("multiaddr d'écoute invalide")?)
                .await?;
            connect_bootstraps(&node, &bootstrap).await?;
            // Annonce un feed du contenu déjà détenu, rediffusé périodiquement.
            let all = node.blockstore().list()?;
            if !all.is_empty() {
                node.publish_feed(&all).await?;
            }
            spawn_feed_republisher(node.clone(), all);
            print_identity(&node, &addr);
            println!("nœud en ligne — Ctrl-C pour arrêter.");
            tokio::signal::ctrl_c().await?;
        }
        Cmd::Add { path, listen } => {
            let node = build_node(&cli.data_dir, &cli.denylist).await?;
            let addr = node
                .listen(listen.parse().context("multiaddr d'écoute invalide")?)
                .await?;
            let bytes = tokio::fs::read(&path)
                .await
                .with_context(|| format!("lecture de {}", path.display()))?;
            let cid = node.add(&bytes).await?;
            println!("CID: {cid}");
            // Annonce un feed signé listant tout le contenu local, rediffusé
            // périodiquement pour les pairs qui se connectent plus tard.
            let all = node.blockstore().list()?;
            node.publish_feed(&all).await?;
            spawn_feed_republisher(node.clone(), all);
            print_identity(&node, &addr);
            println!("contenu publié + feed annoncé — ce nœud le sert. Ctrl-C pour arrêter.");
            tokio::signal::ctrl_c().await?;
        }
        Cmd::Get { cid, peer, out } => {
            let cid: Cid = cid.parse().context("CID invalide")?;
            let node = build_node(&cli.data_dir, &cli.denylist).await?;
            node.listen("/ip4/0.0.0.0/tcp/0".parse().unwrap()).await?;
            connect_peer(&node, &peer).await?;
            let bytes = fetch_with_retry(&node, cid).await?;
            match out {
                Some(p) => {
                    tokio::fs::write(&p, &bytes).await?;
                    println!("{} octets écrits dans {}", bytes.len(), p.display());
                }
                None => println!("{} octets reçus (CID vérifié)", bytes.len()),
            }
        }
        Cmd::FindProviders { cid, peer } => {
            let cid: Cid = cid.parse().context("CID invalide")?;
            let node = build_node(&cli.data_dir, &cli.denylist).await?;
            node.listen("/ip4/0.0.0.0/tcp/0".parse().unwrap()).await?;
            connect_peer(&node, &peer).await?;
            let providers = node.get_providers(cid).await?;
            if providers.is_empty() {
                println!("aucun fournisseur trouvé");
            } else {
                for p in providers {
                    println!("{p}");
                }
            }
        }
        Cmd::Catalog { peer, wait } => {
            let node = build_node(&cli.data_dir, &cli.denylist).await?;
            node.listen("/ip4/0.0.0.0/tcp/0".parse().unwrap()).await?;
            connect_peer(&node, &peer).await?;
            // Écoute les feeds diffusés en gossipsub pendant `wait` secondes.
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            let entries = node.catalog_entries();
            if entries.is_empty() {
                println!("catalogue vide (aucun feed reçu)");
            } else {
                for e in entries {
                    println!("créateur {} (seq {}) :", e.issuer, e.seq);
                    for c in e.cids {
                        println!("  {c}");
                    }
                }
            }
        }
        Cmd::FetchFeed { peer, issuer } => {
            let issuer: champinium_core::PeerId =
                issuer.parse().context("PeerId d'émetteur invalide")?;
            let node = build_node(&cli.data_dir, &cli.denylist).await?;
            node.listen("/ip4/0.0.0.0/tcp/0".parse().unwrap()).await?;
            connect_peer(&node, &peer).await?;
            match fetch_feed_with_retry(&node, issuer).await? {
                Some(feed) => {
                    println!("feed de {issuer} (seq {}) :", feed.seq);
                    for c in feed.cids()? {
                        println!("  {c}");
                    }
                }
                None => println!("aucun feed trouvé dans la DHT pour {issuer}"),
            }
        }
        Cmd::Ingest { path, listen } => {
            let node = build_node(&cli.data_dir, &cli.denylist).await?;
            let addr = node
                .listen(listen.parse().context("multiaddr d'écoute invalide")?)
                .await?;
            let manifest_cid = node.ingest_file(&path).await?;
            println!("Manifeste HLS: {manifest_cid}");
            node.publish_feed(&[manifest_cid]).await?;
            spawn_feed_republisher(node.clone(), vec![manifest_cid]);
            print_identity(&node, &addr);
            println!("média ingéré + feed annoncé — ce nœud le sert. Ctrl-C pour arrêter.");
            tokio::signal::ctrl_c().await?;
        }
        Cmd::FetchHls {
            manifest,
            peer,
            out,
        } => {
            let manifest_cid: Cid = manifest.parse().context("CID de manifeste invalide")?;
            let node = build_node(&cli.data_dir, &cli.denylist).await?;
            node.listen("/ip4/0.0.0.0/tcp/0".parse().unwrap()).await?;
            connect_peer(&node, &peer).await?;
            let playlist = fetch_hls_with_retry(&node, manifest_cid, &out).await?;
            println!("HLS reconstruit: {}", playlist.display());
        }
    }
    Ok(())
}

/// Reconstruit un HLS en retentant le temps que le réseau converge.
async fn fetch_hls_with_retry(node: &Node, manifest: Cid, out: &Path) -> Result<PathBuf> {
    let deadline = std::time::Duration::from_secs(60);
    let start = std::time::Instant::now();
    loop {
        match node.fetch_hls(manifest, out).await {
            Ok(p) => return Ok(p),
            Err(e) if start.elapsed() < deadline => {
                tracing::debug!("fetch-hls en attente ({e}) — nouvelle tentative");
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Rediffuse périodiquement un feed listant `cids` (pour les pairs tardifs).
fn spawn_feed_republisher(node: Node, cids: Vec<Cid>) {
    if cids.is_empty() {
        return;
    }
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            let _ = node.publish_feed(&cids).await;
        }
    });
}

/// Récupère un bloc en retentant le temps que la connexion et Kademlia convergent.
async fn fetch_with_retry(node: &Node, cid: Cid) -> Result<Vec<u8>> {
    let deadline = std::time::Duration::from_secs(20);
    let start = std::time::Instant::now();
    loop {
        match node.get(cid).await {
            Ok(bytes) => return Ok(bytes),
            Err(e) if start.elapsed() < deadline => {
                tracing::debug!("get en attente ({e}) — nouvelle tentative");
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Récupère un feed via la DHT en retentant le temps que le réseau converge.
async fn fetch_feed_with_retry(
    node: &Node,
    issuer: champinium_core::PeerId,
) -> Result<Option<champinium_core::Feed>> {
    let deadline = std::time::Duration::from_secs(20);
    let start = std::time::Instant::now();
    loop {
        if let Some(feed) = node.fetch_feed(issuer).await? {
            return Ok(Some(feed));
        }
        if start.elapsed() >= deadline {
            return Ok(None);
        }
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    }
}

async fn build_node(data_dir: &Path, denylists: &[PathBuf]) -> Result<Node> {
    let kp = load_or_generate(data_dir.join("node.key"))?;
    let bs = Blockstore::open(data_dir.join("blocks"))?;
    // Modération par défaut TOUJOURS active ; on ajoute les souscriptions signées.
    let mut moderation = Moderation::with_default()?;
    for path in denylists {
        let json = std::fs::read_to_string(path)
            .with_context(|| format!("lecture de la denylist {}", path.display()))?;
        let dl = Denylist::from_json(&json)?;
        moderation
            .subscribe(&dl)
            .with_context(|| format!("denylist refusée (signature ?) : {}", path.display()))?;
    }
    Ok(Node::with_moderation(kp, bs, moderation).await?)
}

async fn connect_peer(node: &Node, peer: &str) -> Result<()> {
    let (pid, base) = split_peer_id(peer.parse().context("multiaddr de pair invalide")?)?;
    node.add_address(pid, base.clone()).await?;
    node.dial(base).await?;
    Ok(())
}

async fn connect_bootstraps(node: &Node, bootstraps: &[String]) -> Result<()> {
    for b in bootstraps {
        connect_peer(node, b).await?;
    }
    Ok(())
}

fn print_identity(node: &Node, addr: &champinium_core::Multiaddr) {
    println!("PeerId: {}", node.peer_id());
    println!("Adresse: {addr}/p2p/{}", node.peer_id());
}
