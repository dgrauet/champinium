//! Interface GTK4 + lecture GStreamer (feature `gui`).
//!
//! Pont mince vers le noyau : un runtime tokio exécute les appels async du noyau ;
//! les résultats reviennent sur le thread principal GTK via `glib::spawn_future_local`
//! + un canal oneshot. Aucune logique métier ici.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use champinium_core::{paths, Cid, Node};
use gstreamer::prelude::*;
use gtk::glib;
use gtk::prelude::*;
use gtk::{
    Application, ApplicationWindow, Box as GtkBox, Button, Entry, Label, ListBox, Orientation,
    ScrolledWindow,
};
use tokio::runtime::Runtime;

const APP_ID: &str = "org.champinium.Linux";

/// Point d'entrée de l'interface.
pub fn run() {
    gstreamer::init().expect("initialisation GStreamer");
    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    let _ = app.run();
}

/// État partagé sur le thread principal GTK (non Send : Rc/RefCell).
struct Ui {
    rt: Arc<Runtime>,
    node: RefCell<Option<Node>>,
    player: RefCell<Option<gstreamer::Element>>,
}

fn build_ui(app: &Application) {
    let ui = Rc::new(Ui {
        rt: Arc::new(Runtime::new().expect("runtime tokio")),
        node: RefCell::new(None),
        player: RefCell::new(None),
    });

    let status = Label::new(Some("démarrage…"));
    status.set_xalign(0.0);
    let peer_entry = Entry::builder()
        .placeholder_text("/ip4/…/tcp/…/p2p/<peerid>")
        .hexpand(true)
        .build();
    let connect_btn = Button::with_label("Connecter");
    let refresh_btn = Button::with_label("Rafraîchir");
    let list = ListBox::new();
    let scroller = ScrolledWindow::builder().child(&list).vexpand(true).build();

    let bar = GtkBox::new(Orientation::Horizontal, 8);
    bar.append(&peer_entry);
    bar.append(&connect_btn);
    bar.append(&refresh_btn);

    let root = GtkBox::new(Orientation::Vertical, 8);
    root.set_margin_top(12);
    root.set_margin_bottom(12);
    root.set_margin_start(12);
    root.set_margin_end(12);
    root.append(&status);
    root.append(&bar);
    root.append(&scroller);

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Champinium")
        .default_width(720)
        .default_height(520)
        .child(&root)
        .build();

    // Ouverture du nœud (async).
    {
        let ui = ui.clone();
        let status = status.clone();
        glib::spawn_future_local(async move {
            match open_node(&ui.rt).await {
                Ok(node) => {
                    status.set_text(&format!("nœud en ligne — {}", node.peer_id()));
                    *ui.node.borrow_mut() = Some(node);
                }
                Err(e) => status.set_text(&format!("erreur d'ouverture : {e}")),
            }
        });
    }

    // Connexion à un pair.
    {
        let ui = ui.clone();
        let status = status.clone();
        let peer_entry = peer_entry.clone();
        connect_btn.connect_clicked(move |_| {
            let Some(node) = ui.node.borrow().clone() else {
                status.set_text("nœud pas encore prêt");
                return;
            };
            let peer = peer_entry.text().to_string();
            let rt = ui.rt.clone();
            let status = status.clone();
            glib::spawn_future_local(async move {
                let (tx, rx) = tokio::sync::oneshot::channel();
                rt.spawn(async move {
                    let _ = tx.send(connect_inner(&node, &peer).await);
                });
                match rx.await {
                    Ok(Ok(())) => status.set_text("connecté à un pair"),
                    Ok(Err(e)) => status.set_text(&format!("connexion : {e}")),
                    Err(_) => status.set_text("tâche annulée"),
                }
            });
        });
    }

    // Rafraîchissement du catalogue (lecture synchrone).
    {
        let ui = ui.clone();
        let status = status.clone();
        let list = list.clone();
        refresh_btn.connect_clicked(move |_| {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            let Some(node) = ui.node.borrow().clone() else {
                return;
            };
            let entries = node.catalog_entries();
            status.set_text(&format!("catalogue : {} créateur(s)", entries.len()));
            for entry in entries {
                for cid in entry.cids {
                    list.append(&catalog_row(&ui, &status, &cid.to_string()));
                }
            }
        });
    }

    window.present();
}

/// Une ligne du catalogue : le CID + un bouton « Lire ».
fn catalog_row(ui: &Rc<Ui>, status: &Label, cid: &str) -> GtkBox {
    let row = GtkBox::new(Orientation::Horizontal, 8);
    let label = Label::new(Some(cid));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    let play = Button::with_label("Lire");
    row.append(&label);
    row.append(&play);

    let ui = ui.clone();
    let status = status.clone();
    let cid = cid.to_string();
    play.connect_clicked(move |_| {
        let Some(node) = ui.node.borrow().clone() else {
            return;
        };
        let Ok(manifest) = cid.parse::<Cid>() else {
            status.set_text("CID invalide");
            return;
        };
        let rt = ui.rt.clone();
        let ui = ui.clone();
        let status = status.clone();
        status.set_text("récupération…");
        glib::spawn_future_local(async move {
            let (tx, rx) = tokio::sync::oneshot::channel();
            rt.spawn(async move {
                let _ = tx.send(fetch_inner(&node, manifest).await);
            });
            match rx.await {
                Ok(Ok(playlist)) => match start_playback(&playlist) {
                    Ok(player) => {
                        // Arrête la lecture précédente, conserve la nouvelle vivante.
                        if let Some(old) = ui.player.borrow_mut().take() {
                            let _ = old.set_state(gstreamer::State::Null);
                        }
                        *ui.player.borrow_mut() = Some(player);
                        status.set_text("lecture en cours");
                    }
                    Err(e) => status.set_text(&format!("lecture : {e}")),
                },
                Ok(Err(e)) => status.set_text(&format!("récupération : {e}")),
                Err(_) => status.set_text("tâche annulée"),
            }
        });
    });
    row
}

/// Démarre une lecture GStreamer (playbin + fenêtre vidéo par défaut).
fn start_playback(playlist: &Path) -> Result<gstreamer::Element, String> {
    let uri = format!("file://{}", playlist.display());
    let playbin = gstreamer::ElementFactory::make("playbin")
        .property("uri", &uri)
        .build()
        .map_err(|e| format!("playbin indisponible : {e}"))?;
    playbin
        .set_state(gstreamer::State::Playing)
        .map_err(|e| format!("démarrage : {e}"))?;
    Ok(playbin)
}

// --- Ponts vers le noyau (exécutés sur le runtime tokio) ---

async fn open_node(rt: &Arc<Runtime>) -> Result<Node, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    rt.spawn(async move {
        // Répertoire durable par OS (jamais le tmp : sinon perte du PeerId).
        let res = async {
            let dir = paths::default_data_dir();
            let node = Node::open(&dir).await.map_err(|e| e.to_string())?;
            // Écoute pour être joignable (reseed entrant, statut seeder).
            node.listen(
                "/ip4/0.0.0.0/tcp/0"
                    .parse()
                    .map_err(|e| format!("multiaddr d'écoute invalide : {e}"))?,
            )
            .await
            .map_err(|e| e.to_string())?;
            Ok(node)
        }
        .await;
        let _ = tx.send(res);
    });
    rx.await.map_err(|_| "tâche annulée".to_string())?
}

async fn connect_inner(node: &Node, peer: &str) -> Result<(), String> {
    let addr = peer
        .parse()
        .map_err(|e| format!("multiaddr invalide : {e}"))?;
    node.connect(addr).await.map_err(|e| e.to_string())
}

async fn fetch_inner(node: &Node, manifest: Cid) -> Result<PathBuf, String> {
    let out = std::env::temp_dir().join(format!("champinium-play-{manifest}"));
    node.fetch_hls(manifest, &out)
        .await
        .map_err(|e| e.to_string())
}
