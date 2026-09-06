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
use champinium_core::{channel_link, Cid, Denylist, Node, PeerId};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "champinium-cli", about = "Debug CLI du noyau Champinium")]
struct Cli {
    /// Répertoire de données du nœud (clé d'identité + blocs).
    #[arg(long, default_value = ".champinium")]
    data_dir: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Affiche le PeerId du nœud.
    Id,
    /// Démarre un nœud qui écoute (et se connecte à des bootstrap), puis reste
    /// en ligne. Le feed rediffusé réutilise les métadonnées déjà déclarées
    /// (titre/tags/provenance) pour chaque CID connu ; seuls les CIDs jamais
    /// déclarés apparaissent en « non déclaré ».
    Serve {
        #[arg(long, default_value = "/ip4/0.0.0.0/tcp/0")]
        listen: String,
        /// Adresses de bootstrap (`/ip4/.../tcp/.../p2p/<peerid>`), répétable.
        #[arg(long)]
        bootstrap: Vec<String>,
    },
    /// Publie un fichier (stocke + annonce dans la DHT), puis reste en ligne
    /// pour le servir. Le feed rediffusé réutilise les métadonnées déjà
    /// déclarées (titre/tags/provenance) pour chaque CID connu ; seuls les
    /// CIDs jamais déclarés apparaissent en « non déclaré ».
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
    /// Facteur de réplication mesuré d'un CID (nombre de fournisseurs DHT).
    Replication {
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
        /// Affiche uniquement les créateurs souscrits (sinon: catalogue complet).
        #[arg(long)]
        subscribed: bool,
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
        /// Titre du contenu (métadonnée signée, cherchable par les pairs).
        #[arg(long, default_value = "")]
        title: String,
        /// Tag du contenu (répétable ; normalisé en minuscules, cherchable).
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// Déclaration de provenance (obligatoire, signée avec le feed) :
        /// generated | assisted | captured | undeclared.
        #[arg(long, value_parser = parse_provenance_mode)]
        provenance: champinium_core::feed::ProvenanceMode,
        /// Outil ou modèle utilisé (répétable, ≤ 8 ; normalisé, cherchable).
        #[arg(long = "tool")]
        tools: Vec<String>,
    },
    /// Recherche locale (titres/tags du catalogue reconstruit en écoutant).
    Search {
        query: String,
        /// Pair auquel se connecter `/ip4/.../tcp/.../p2p/<peerid>`.
        #[arg(long)]
        peer: String,
        /// Durée d'écoute des feeds avant la recherche (secondes).
        #[arg(long, default_value = "6")]
        wait: u64,
    },
    /// Recherche par tag via la DHT (découverte hors gossip).
    SearchTag {
        tag: String,
        #[arg(long)]
        peer: String,
    },
    /// Export hors ligne : télécharge TOUT un HLS (manifeste + segments) dans un répertoire. Pour lire, préférer `stream`.
    FetchHls {
        /// CID du manifeste HLS.
        manifest: String,
        #[arg(long)]
        peer: String,
        /// Répertoire de sortie (recevra index.m3u8 + segments).
        #[arg(long)]
        out: PathBuf,
    },
    /// Lecture progressive : ouvre une session HLS locale et affiche son URL
    /// (à donner à ffplay/VLC) ; reste en vie jusqu'à Ctrl-C.
    Stream {
        /// CID du manifeste HLS.
        manifest: String,
        #[arg(long)]
        peer: String,
    },
    /// S'abonne à un créateur (par lien ou PeerId nu).
    Subscribe {
        /// Lien `champinium://channel/<peerid>` ou PeerId nu.
        link_or_peerid: String,
        /// Adresse du pair `/ip4/.../tcp/.../p2p/<peerid>` (optionnel, pour le fetch immédiat).
        #[arg(long)]
        peer: Option<String>,
    },
    /// Se désabonne d'un créateur.
    Unsubscribe {
        /// PeerId du créateur.
        peerid: String,
    },
    /// Liste les créateurs souscrits avec leurs liens.
    Subscriptions,
    /// Bloque un créateur localement (par lien ou PeerId nu) — préférence
    /// privée, jamais publiée.
    Block {
        /// Lien `champinium://channel/<peerid>` ou PeerId nu.
        link_or_peerid: String,
    },
    /// Débloque un créateur bloqué localement.
    Unblock {
        /// PeerId du créateur.
        peerid: String,
    },
    /// Liste les créateurs bloqués localement avec leurs liens.
    Blocked,
    /// Affiche (et éventuellement définit) le quota de seed proactif.
    Quota {
        /// Nouveau quota en octets (sinon: affiche seulement l'état courant).
        #[arg(long)]
        set: Option<u64>,
    },
    /// Épingle un manifeste (exempté d'éviction par le seed proactif).
    Pin {
        /// CID du manifeste.
        manifest_cid: String,
    },
    /// Retire l'épinglage d'un manifeste (redevient évictable sous quota).
    Unpin {
        /// CID du manifeste.
        manifest_cid: String,
    },
    /// Affiche les signalements agrégés localement (matière première pour un
    /// éditeur de denylist — aucun effet automatique).
    Reports {
        /// Regroupe par émetteur (jointure locale rapports × catalogue)
        /// au lieu des compteurs globaux par CID.
        #[arg(long)]
        by_channel: bool,
    },
    /// Affiche (et éventuellement définit) le débrayage du repli de
    /// récupération froid.
    #[cfg(feature = "cold-storage")]
    ColdRetrieval {
        /// `on` ou `off`.
        #[arg(long)]
        set: Option<String>,
    },
    /// Listes de modération : suivi d'éditeurs, outillage éditeur.
    Denylist {
        #[command(subcommand)]
        action: DenylistCmd,
    },
}

#[derive(Subcommand)]
enum DenylistCmd {
    /// État des éditeurs suivis (l'éditeur projet est toujours présent).
    Sources,
    /// Suit un éditeur (lien champinium://denylist/<peerid> ou PeerId nu).
    Follow {
        link_or_peerid: String,
        #[arg(long)]
        peer: Option<String>,
    },
    /// Cesse de suivre un éditeur tiers (l'éditeur projet ne peut pas l'être).
    Unfollow { peerid: String },
    /// Signe une liste HORS nœud avec une clé d'éditeur (générée si absente).
    Sign {
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(long)]
        seq: u64,
        #[arg(long = "cid")]
        cids: Vec<String>,
        #[arg(long = "key-entry")]
        key_entries: Vec<String>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Publie une liste signée dans la DHT depuis ce nœud.
    Publish {
        file: PathBuf,
        #[arg(long)]
        peer: String,
    },
    /// Vérifie et affiche une liste signée.
    Show { file: PathBuf },
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
            let node = build_node(&cli.data_dir).await?;
            let addr = node
                .listen(listen.parse().context("multiaddr d'écoute invalide")?)
                .await?;
            connect_bootstraps(&node, &bootstrap).await?;
            // Annonce un feed du contenu déjà détenu, rediffusé périodiquement,
            // en réutilisant les métadonnées déjà déclarées pour chaque CID
            // connu (jamais d'écrasement d'une déclaration existante).
            let all = node.blockstore().list()?;
            if !all.is_empty() {
                let entries = own_entries_for(&node, &all).await;
                node.publish_feed_with(&entries).await?;
                spawn_feed_republisher_with(node.clone(), entries);
            }
            print_identity(&node, &addr);
            println!("nœud en ligne — Ctrl-C pour arrêter.");
            tokio::signal::ctrl_c().await?;
        }
        Cmd::Add { path, listen } => {
            let node = build_node(&cli.data_dir).await?;
            let addr = node
                .listen(listen.parse().context("multiaddr d'écoute invalide")?)
                .await?;
            let bytes = tokio::fs::read(&path)
                .await
                .with_context(|| format!("lecture de {}", path.display()))?;
            let cid = node.add(&bytes).await?;
            println!("CID: {cid}");
            // Annonce un feed signé listant tout le contenu local, rediffusé
            // périodiquement pour les pairs qui se connectent plus tard, en
            // réutilisant les métadonnées déjà déclarées pour chaque CID connu
            // (jamais d'écrasement d'une déclaration existante).
            let all = node.blockstore().list()?;
            let entries = own_entries_for(&node, &all).await;
            node.publish_feed_with(&entries).await?;
            spawn_feed_republisher_with(node.clone(), entries);
            print_identity(&node, &addr);
            println!("contenu publié + feed annoncé — ce nœud le sert. Ctrl-C pour arrêter.");
            tokio::signal::ctrl_c().await?;
        }
        Cmd::Get { cid, peer, out } => {
            let cid: Cid = cid.parse().context("CID invalide")?;
            let node = build_node(&cli.data_dir).await?;
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
            let node = build_node(&cli.data_dir).await?;
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
        Cmd::Replication { cid, peer } => {
            let cid: Cid = cid.parse().context("CID invalide")?;
            let node = build_node(&cli.data_dir).await?;
            node.listen("/ip4/0.0.0.0/tcp/0".parse().unwrap()).await?;
            connect_peer(&node, &peer).await?;
            let n = node.replication_factor(cid).await?;
            println!("facteur de réplication: {n} fournisseur(s)");
        }
        Cmd::Catalog {
            peer,
            wait,
            subscribed,
        } => {
            let node = build_node(&cli.data_dir).await?;
            node.listen("/ip4/0.0.0.0/tcp/0".parse().unwrap()).await?;
            connect_peer(&node, &peer).await?;
            // Écoute les feeds diffusés en gossipsub pendant `wait` secondes.
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            let entries = if subscribed {
                node.catalog_subscribed()
            } else {
                node.catalog_entries()
            };
            if entries.is_empty() {
                let msg = if subscribed {
                    "aucun créateur souscrit avec contenu"
                } else {
                    "catalogue vide (aucun feed reçu)"
                };
                println!("{msg}");
            } else {
                for e in entries {
                    println!("créateur {} (seq {}) :", e.issuer, e.seq);
                    for item in &e.items {
                        let title = if item.title.is_empty() {
                            "(sans titre)"
                        } else {
                            &item.title
                        };
                        println!(
                            "  {} {title} — {}",
                            provenance_label(&item.provenance),
                            item.cid
                        );
                    }
                }
            }
        }
        Cmd::FetchFeed { peer, issuer } => {
            let issuer: champinium_core::PeerId =
                issuer.parse().context("PeerId d'émetteur invalide")?;
            let node = build_node(&cli.data_dir).await?;
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
        Cmd::Ingest {
            path,
            listen,
            title,
            tags,
            provenance,
            tools,
        } => {
            if tools.len() > champinium_core::feed::MAX_TOOLS_PER_ENTRY {
                anyhow::bail!(
                    "au plus {} outils",
                    champinium_core::feed::MAX_TOOLS_PER_ENTRY
                );
            }
            let node = build_node(&cli.data_dir).await?;
            let addr = node
                .listen(listen.parse().context("multiaddr d'écoute invalide")?)
                .await?;
            let manifest_cid = node.ingest_file(&path).await?;
            println!("Manifeste HLS: {manifest_cid}");
            let entry = champinium_core::feed::FeedEntry {
                cid: manifest_cid.to_string(),
                title,
                tags,
                provenance: champinium_core::feed::Provenance {
                    mode: provenance,
                    tools,
                },
            };
            node.publish_feed_with(std::slice::from_ref(&entry)).await?;
            spawn_feed_republisher_with(node.clone(), vec![entry]);
            print_identity(&node, &addr);
            println!("média ingéré + feed annoncé — ce nœud le sert. Ctrl-C pour arrêter.");
            tokio::signal::ctrl_c().await?;
        }
        Cmd::Search { query, peer, wait } => {
            let node = build_node(&cli.data_dir).await?;
            node.listen("/ip4/0.0.0.0/tcp/0".parse().unwrap()).await?;
            connect_peer(&node, &peer).await?;
            // Laisse le catalogue se reconstruire par écoute gossip.
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            print_hits(&node.search(&query));
        }
        Cmd::SearchTag { tag, peer } => {
            let node = build_node(&cli.data_dir).await?;
            node.listen("/ip4/0.0.0.0/tcp/0".parse().unwrap()).await?;
            connect_peer(&node, &peer).await?;
            let hits = search_tag_with_retry(&node, &tag).await?;
            print_hits(&hits);
        }
        Cmd::FetchHls {
            manifest,
            peer,
            out,
        } => {
            let manifest_cid: Cid = manifest.parse().context("CID de manifeste invalide")?;
            let node = build_node(&cli.data_dir).await?;
            node.listen("/ip4/0.0.0.0/tcp/0".parse().unwrap()).await?;
            connect_peer(&node, &peer).await?;
            let playlist = fetch_hls_with_retry(&node, manifest_cid, &out).await?;
            println!("HLS reconstruit: {}", playlist.display());
        }
        Cmd::Stream { manifest, peer } => {
            let manifest_cid: Cid = manifest.parse().context("CID de manifeste invalide")?;
            let node = build_node(&cli.data_dir).await?;
            node.listen("/ip4/0.0.0.0/tcp/0".parse().unwrap()).await?;
            connect_peer(&node, &peer).await?;
            let info = open_stream_with_retry(&node, manifest_cid).await?;
            println!("{}", info.url);
            eprintln!(
                "session {} — {} segment(s) — Ctrl-C pour fermer",
                info.id, info.total_segments
            );
            let mut events = node.subscribe_stream();
            loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => break,
                    r = events.recv() => {
                        match r {
                            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                if let Ok(st) = node.stream_status(info.id) {
                                    match st.failed_reason {
                                        Some(reason) => eprintln!("échec: {reason}"),
                                        None => eprintln!("{}/{}", st.fetched_segments, st.total_segments),
                                    }
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
            node.close_stream(info.id).await;
            eprintln!("session fermée");
        }
        Cmd::Subscribe {
            link_or_peerid,
            peer,
        } => {
            let issuer: PeerId =
                channel_link::parse(&link_or_peerid).context("lien ou PeerId invalide")?;
            let node = build_node(&cli.data_dir).await?;
            node.listen("/ip4/0.0.0.0/tcp/0".parse().unwrap()).await?;
            if let Some(peer_addr) = peer {
                connect_peer(&node, &peer_addr).await?;
            }
            node.subscribe(issuer)?;
            println!("abonné à {}", channel_link::format(&issuer));
        }
        Cmd::Unsubscribe { peerid } => {
            let issuer: PeerId = peerid.parse().context("PeerId invalide")?;
            let node = build_node(&cli.data_dir).await?;
            node.unsubscribe(issuer)?;
            println!("désabonné de {}", channel_link::format(&issuer));
        }
        Cmd::Subscriptions => {
            let node = build_node(&cli.data_dir).await?;
            let subs = node.subscriptions();
            if subs.is_empty() {
                println!("aucun abonnement");
            } else {
                for issuer in subs {
                    println!("{}", channel_link::format(&issuer));
                }
            }
        }
        Cmd::Block { link_or_peerid } => {
            let issuer: PeerId =
                channel_link::parse(&link_or_peerid).context("lien ou PeerId invalide")?;
            let node = build_node(&cli.data_dir).await?;
            node.block_channel(issuer).await?;
            println!("bloqué: {}", channel_link::format(&issuer));
        }
        Cmd::Unblock { peerid } => {
            let issuer: PeerId = peerid.parse().context("PeerId invalide")?;
            let node = build_node(&cli.data_dir).await?;
            node.unblock_channel(issuer)?;
            println!("débloqué: {}", channel_link::format(&issuer));
        }
        Cmd::Blocked => {
            let node = build_node(&cli.data_dir).await?;
            let blocked = node.blocked_channels();
            if blocked.is_empty() {
                println!("aucun channel bloqué");
            } else {
                for issuer in blocked {
                    println!("{}", channel_link::format(&issuer));
                }
            }
        }
        Cmd::Quota { set } => {
            let node = build_node(&cli.data_dir).await?;
            if let Some(bytes) = set {
                node.set_seed_quota(bytes)?;
            }
            let (used, quota) = node.storage_stats();
            println!("utilisé: {used} octet(s) / quota: {quota} octet(s)");
        }
        Cmd::Pin { manifest_cid } => {
            let manifest_cid: Cid = manifest_cid.parse().context("CID de manifeste invalide")?;
            let node = build_node(&cli.data_dir).await?;
            node.pin(manifest_cid)?;
            println!("épinglé: {manifest_cid}");
        }
        Cmd::Unpin { manifest_cid } => {
            let manifest_cid: Cid = manifest_cid.parse().context("CID de manifeste invalide")?;
            let node = build_node(&cli.data_dir).await?;
            node.unpin(manifest_cid)?;
            println!("épinglage retiré: {manifest_cid}");
        }
        Cmd::Reports { by_channel } => {
            let node = build_node(&cli.data_dir).await?;
            if by_channel {
                let by_channel = node.report_counts_by_channel();
                if by_channel.is_empty() {
                    println!("aucun signalement attribuable à un émetteur connu");
                } else {
                    for (issuer, reporters, cids) in by_channel {
                        println!("{issuer}: {reporters} rapporteur(s) distinct(s) / {cids} CID(s) signalé(s)");
                    }
                }
            } else {
                let counts = node.report_counts();
                if counts.is_empty() {
                    println!("aucun signalement");
                } else {
                    for (cid, reporters) in counts {
                        println!("{cid}: {reporters} rapporteur(s) distinct(s)");
                    }
                }
            }
        }
        #[cfg(feature = "cold-storage")]
        Cmd::ColdRetrieval { set } => {
            let node = build_node(&cli.data_dir).await?;
            if let Some(value) = set {
                let enabled = match value.to_lowercase().as_str() {
                    "on" => true,
                    "off" => false,
                    other => anyhow::bail!("valeur invalide pour --set: {other} (attendu on|off)"),
                };
                node.set_cold_retrieval(enabled)?;
            }
            let state = if node.cold_retrieval_enabled() {
                "activé"
            } else {
                "désactivé"
            };
            println!("repli de récupération froid: {state}");
        }
        Cmd::Denylist { action } => match action {
            DenylistCmd::Sources => {
                let node = build_node(&cli.data_dir).await?;
                let project = node.project_issuer();
                for issuer in node.denylist_issuers() {
                    let lock = if Some(issuer) == project {
                        " [projet, verrouillée]"
                    } else {
                        ""
                    };
                    match node.denylist_source(&issuer) {
                        Some(s) => println!(
                            "{issuer}{lock} — {} — seq {} — {} CID(s), {} clé(s) — {}",
                            s.name, s.seq, s.entry_count, s.key_count, s.updated
                        ),
                        None => println!("{issuer}{lock} — jamais récupérée"),
                    }
                }
            }
            DenylistCmd::Follow {
                link_or_peerid,
                peer,
            } => {
                let issuer = channel_link::parse_denylist(&link_or_peerid)
                    .context("lien ou PeerId invalide")?;
                let node = build_node(&cli.data_dir).await?;
                node.listen("/ip4/0.0.0.0/tcp/0".parse().unwrap()).await?;
                if let Some(p) = peer {
                    connect_peer(&node, &p).await?;
                }
                node.subscribe_denylist_issuer(issuer)?;
                match node.fetch_denylist(issuer).await? {
                    Some(l) => println!("suivi: {issuer} — liste « {} » seq {}", l.name, l.seq),
                    None => println!(
                        "suivi: {issuer} — aucune liste trouvée pour l'instant (réessai périodique)"
                    ),
                }
            }
            DenylistCmd::Unfollow { peerid } => {
                let issuer: PeerId = peerid.parse().context("PeerId invalide")?;
                let node = build_node(&cli.data_dir).await?;
                node.unsubscribe_denylist_issuer(issuer).await?;
                println!("ne suit plus: {issuer}");
            }
            DenylistCmd::Sign {
                key,
                name,
                seq,
                cids,
                key_entries,
                out,
            } => {
                let created = !key.exists();
                let kp = load_or_generate(&key)?;
                if created {
                    eprintln!(
                        "nouvelle clé d'éditeur écrite dans {} — PeerId à diffuser : {}",
                        key.display(),
                        kp.public().to_peer_id()
                    );
                }
                let cids: Vec<Cid> = cids
                    .iter()
                    .map(|c| c.parse().context("CID invalide"))
                    .collect::<Result<_>>()?;
                let keys: Vec<PeerId> = key_entries
                    .iter()
                    .map(|k| k.parse().context("PeerId invalide"))
                    .collect::<Result<_>>()?;
                let updated = rfc3339_now();
                let dl = Denylist::build_signed(&name, &updated, &kp, seq, &cids, &keys)?;
                std::fs::write(&out, serde_json::to_string_pretty(&dl)?)?;
                println!(
                    "liste signée: {} (éditeur {}, seq {seq}, {} CID(s), {} clé(s))",
                    out.display(),
                    kp.public().to_peer_id(),
                    cids.len(),
                    keys.len()
                );
            }
            DenylistCmd::Publish { file, peer } => {
                let dl = Denylist::from_json(&std::fs::read_to_string(&file)?)?;
                dl.verify().context("liste refusée (signature ?)")?;
                let node = build_node(&cli.data_dir).await?;
                node.listen("/ip4/0.0.0.0/tcp/0".parse().unwrap()).await?;
                connect_peer(&node, &peer).await?;
                node.publish_denylist(&dl).await?;
                println!(
                    "publiée: éditeur {} seq {} — laisser tourner quelques secondes pour la propagation DHT",
                    dl.issuer_peer_id()?,
                    dl.seq
                );
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
            DenylistCmd::Show { file } => {
                let dl = Denylist::from_json(&std::fs::read_to_string(&file)?)?;
                dl.verify().context("liste refusée (signature ?)")?;
                println!(
                    "{} — éditeur {} — seq {} — {} CID(s), {} clé(s) — {}",
                    dl.name,
                    dl.issuer_peer_id()?,
                    dl.seq,
                    dl.entries.len(),
                    dl.key_entries.len(),
                    dl.updated
                );
            }
        },
    }
    Ok(())
}

/// Horodatage RFC 3339 courant (UTC), pour les métadonnées de liste signée.
fn rfc3339_now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
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

/// Ouvre une session de lecture en retentant le temps que le réseau converge.
async fn open_stream_with_retry(
    node: &Node,
    manifest: Cid,
) -> Result<champinium_core::StreamSessionInfo> {
    let deadline = std::time::Duration::from_secs(60);
    let start = std::time::Instant::now();
    loop {
        match node.open_stream(manifest).await {
            Ok(i) => return Ok(i),
            Err(e) if start.elapsed() < deadline => {
                tracing::debug!("stream en attente ({e}) — nouvelle tentative");
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Construit les entrées de feed pour `cids` en réutilisant les métadonnées
/// (titre/tags/provenance) déjà déclarées par ce nœud pour chaque CID connu.
/// Un CID jamais déclaré retombe sur `FeedEntry::undeclared` — jamais
/// l'inverse : une déclaration existante n'est jamais écrasée par `serve`/
/// `add` (voir revue finale, point Important 1).
async fn own_entries_for(node: &Node, cids: &[Cid]) -> Vec<champinium_core::feed::FeedEntry> {
    let self_id = node.peer_id();
    let mut own = node
        .catalog_entries()
        .into_iter()
        .find(|e| e.issuer == self_id);
    if own.is_none() {
        // Après un redémarrage, le catalogue local peut ne pas encore
        // connaître le propre feed du nœud (il ne vit que dans la DHT tant
        // qu'aucun feed n'a été republié) : une tentative de récupération.
        // `fetch_feed` alimente le catalogue lui-même quand il trouve un feed.
        if let Ok(Some(_)) = node.fetch_feed(self_id).await {
            own = node
                .catalog_entries()
                .into_iter()
                .find(|e| e.issuer == self_id);
        }
    }
    let by_cid: std::collections::HashMap<String, &champinium_core::catalog::CatalogItem> = own
        .as_ref()
        .map(|e| e.items.iter().map(|i| (i.cid.to_string(), i)).collect())
        .unwrap_or_default();
    cids.iter()
        .map(|cid| {
            let key = cid.to_string();
            match by_cid.get(&key) {
                Some(item) => champinium_core::feed::FeedEntry {
                    cid: key,
                    title: item.title.clone(),
                    tags: item.tags.clone(),
                    provenance: item.provenance.clone(),
                },
                None => {
                    champinium_core::feed::FeedEntry::undeclared(key, String::new(), Vec::new())
                }
            }
        })
        .collect()
}

/// Rediffuse périodiquement un feed avec métadonnées (titre/tags/provenance).
fn spawn_feed_republisher_with(node: Node, entries: Vec<champinium_core::feed::FeedEntry>) {
    if entries.is_empty() {
        return;
    }
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            let _ = node.publish_feed_with(&entries).await;
        }
    });
}

fn parse_provenance_mode(s: &str) -> Result<champinium_core::feed::ProvenanceMode, String> {
    champinium_core::feed::ProvenanceMode::parse(s).ok_or_else(|| {
        format!(
            "provenance invalide '{s}' (attendu : generated | assisted | captured | undeclared)"
        )
    })
}

/// Préfixe court d'affichage d'un mode de provenance.
fn provenance_label(p: &champinium_core::feed::Provenance) -> String {
    use champinium_core::feed::ProvenanceMode as M;
    let mode = match p.mode {
        M::Generated => "IA",
        M::Assisted => "IA assistée",
        M::Captured => "capturé",
        M::Undeclared => "non déclaré",
    };
    if p.tools.is_empty() {
        format!("[{mode}]")
    } else {
        format!("[{mode} · {}]", p.tools.join(", "))
    }
}

/// Affiche des résultats de recherche.
fn print_hits(hits: &[champinium_core::catalog::SearchHit]) {
    if hits.is_empty() {
        println!("aucun résultat");
        return;
    }
    for h in hits {
        let title = if h.title.is_empty() {
            "(sans titre)"
        } else {
            &h.title
        };
        println!(
            "{} {title} — {} (créateur {}) [{}]",
            provenance_label(&h.provenance),
            h.cid,
            h.issuer,
            h.tags.join(", ")
        );
    }
}

/// Recherche par tag en retentant le temps que le réseau converge.
async fn search_tag_with_retry(
    node: &Node,
    tag: &str,
) -> Result<Vec<champinium_core::catalog::SearchHit>> {
    let deadline = std::time::Duration::from_secs(20);
    let start = std::time::Instant::now();
    loop {
        let hits = node.search_tag(tag).await?;
        if !hits.is_empty() || start.elapsed() >= deadline {
            return Ok(hits);
        }
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    }
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

async fn build_node(data_dir: &Path) -> Result<Node> {
    Ok(Node::open(data_dir).await?)
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
