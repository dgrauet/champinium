//! Interface GTK4 + lecture GStreamer (feature `gui`).
//!
//! Pont mince vers le noyau : un runtime tokio exécute les appels async (et les
//! appels sync à I/O disque, comme `subscribe`/`unsubscribe`) du noyau ; les
//! résultats reviennent sur le thread principal GTK via `glib::spawn_future_local`
//! + un canal oneshot. Aucune logique métier ici.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use champinium_core::p2p::ChannelPreview;
use champinium_core::{channel_link, paths, CatalogEntry, Cid, CoreError, Node, PeerId};
use gstreamer::prelude::*;
use gtk::glib;
use gtk::prelude::*;
use gtk::{
    Application, ApplicationWindow, Box as GtkBox, Button, ButtonsType, Entry, Label, ListBox,
    MessageDialog, Orientation, ResponseType, ScrolledWindow, Spinner, Stack, StackSwitcher,
};
use tokio::runtime::Runtime;

const APP_ID: &str = "org.champinium.Linux";

/// Texte de l'avertissement au premier passage sur l'onglet Explorer (identique
/// aux fronts macOS/Windows — spec channels).
const EXPLORER_WARNING: &str =
    "Contenu non curé venant du réseau ouvert, filtré uniquement par les denylists.";

/// Point d'entrée de l'interface.
pub fn run() {
    gstreamer::init().expect("initialisation GStreamer");
    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    let _ = app.run();
}

/// Racine des répertoires de lecture temporaires (un sous-dossier par contenu).
fn play_root() -> PathBuf {
    std::env::temp_dir().join("champinium-play")
}

/// État partagé sur le thread principal GTK (non Send : Rc/RefCell).
struct Ui {
    rt: Arc<Runtime>,
    node: RefCell<Option<Node>>,
    player: RefCell<Option<gstreamer::Element>>,
    /// Répertoire de la lecture en cours (supprimé au changement de contenu).
    current_play_dir: RefCell<Option<PathBuf>>,
    /// Avertissement Explorer déjà accepté cette session (mémoire de session
    /// uniquement — pas de GSettings, YAGNI, voir brief tâche 7).
    explorer_warned: Cell<bool>,
    /// Fenêtre principale — posée juste après sa construction (elle n'existe
    /// pas encore quand `Ui` est créé). Sert de `transient_for` aux dialogues
    /// ouverts depuis un en-tête de channel (confirmation de blocage).
    window: RefCell<Option<ApplicationWindow>>,
}

fn build_ui(app: &Application) {
    // Purge les répertoires de lecture des exécutions précédentes (ils ne
    // servent qu'à la session en cours et s'accumuleraient sinon).
    let _ = std::fs::remove_dir_all(play_root());

    let ui = Rc::new(Ui {
        rt: Arc::new(Runtime::new().expect("runtime tokio")),
        node: RefCell::new(None),
        player: RefCell::new(None),
        current_play_dir: RefCell::new(None),
        explorer_warned: Cell::new(false),
        window: RefCell::new(None),
    });

    let status = Label::new(Some("démarrage…"));
    status.set_xalign(0.0);
    status.set_hexpand(true);
    // Réglages de seed (lot c) : quota (Go) + usage courant — popup dédiée
    // pour rester cohérente avec la vue unique existante (pas de fenêtre de
    // préférences séparée).
    let seed_settings_btn = Button::with_label("Réglages de seed");
    // Réglages : liste des channels bloqués localement (tâche d/6) — même
    // patron de popup dédiée que "Réglages de seed".
    let blocked_channels_btn = Button::with_label("Channels bloqués");
    let header_bar = GtkBox::new(Orientation::Horizontal, 8);
    header_bar.append(&status);
    header_bar.append(&seed_settings_btn);
    header_bar.append(&blocked_channels_btn);

    let peer_entry = Entry::builder()
        .placeholder_text("/ip4/…/tcp/…/p2p/<peerid>")
        .hexpand(true)
        .build();
    let connect_btn = Button::with_label("Connecter");
    let search_entry = Entry::builder()
        .placeholder_text("Rechercher (titre ou tag)…")
        .build();

    let bar = GtkBox::new(Orientation::Horizontal, 8);
    bar.append(&peer_entry);
    bar.append(&connect_btn);

    // Onglets Abonnements (défaut) / Explorer — deux ListBox alimentées par
    // `refresh_lists()` depuis le core (`catalog_subscribed()` /
    // `catalog_entries()`).
    let subs_list = ListBox::new();
    let subs_scroller = ScrolledWindow::builder()
        .child(&subs_list)
        .vexpand(true)
        .build();

    let explorer_list = ListBox::new();
    let explorer_scroller = ScrolledWindow::builder()
        .child(&explorer_list)
        .vexpand(true)
        .build();

    // Abonnement par lien + copie du lien de mon propre channel — visibles
    // uniquement dans l'onglet Explorer (même choix que macOS/Windows).
    let channel_entry = Entry::builder()
        .placeholder_text("Lien de channel ou PeerId…")
        .hexpand(true)
        .build();
    // « Aperçu » résout le lien/PeerId SANS abonner (l'ancien abonnement
    // direct au collage est supprimé, tâche 3) — l'abonnement se décide
    // depuis la feuille d'aperçu (`open_channel_preview`).
    let preview_link_btn = Button::with_label("Aperçu");
    let preview_spinner = Spinner::new();
    let copy_link_btn = Button::with_label("Copier le lien de mon channel");
    let link_msg = Label::new(None);
    link_msg.set_xalign(0.0);
    link_msg.add_css_class("dim-label");

    let explorer_toolbar = GtkBox::new(Orientation::Horizontal, 8);
    explorer_toolbar.append(&channel_entry);
    explorer_toolbar.append(&preview_spinner);
    explorer_toolbar.append(&preview_link_btn);
    explorer_toolbar.append(&copy_link_btn);

    let explorer_page = GtkBox::new(Orientation::Vertical, 6);
    explorer_page.append(&explorer_toolbar);
    explorer_page.append(&link_msg);
    explorer_page.append(&explorer_scroller);

    let stack = Stack::new();
    stack.add_titled(&subs_scroller, Some("abonnements"), "Abonnements");
    stack.add_titled(&explorer_page, Some("explorer"), "Explorer");
    stack.set_visible_child_name("abonnements");
    stack.set_vexpand(true);

    let switcher = StackSwitcher::new();
    switcher.set_stack(Some(&stack));

    let root = GtkBox::new(Orientation::Vertical, 8);
    root.set_margin_top(12);
    root.set_margin_bottom(12);
    root.set_margin_start(12);
    root.set_margin_end(12);
    root.append(&header_bar);
    root.append(&bar);
    root.append(&search_entry);
    root.append(&switcher);
    root.append(&stack);

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Champinium")
        .default_width(720)
        .default_height(520)
        .child(&root)
        .build();

    // Posée une fois construite — sert de `transient_for` aux dialogues
    // ouverts depuis un en-tête de channel (voir `Ui::window`).
    *ui.window.borrow_mut() = Some(window.clone());

    // Premier passage sur Explorer : avertissement bloquant, mémorisé pour la
    // session (voir `Ui::explorer_warned`). Le passage est annulé (retour sur
    // Abonnements) tant que l'avertissement n'a pas été accepté.
    {
        let ui = ui.clone();
        let window = window.clone();
        // Méthode générée par gtk4 directement sur `Stack` (pas de trait
        // générique `ObjectExt`) : évite l'ambiguïté d'inférence entre le
        // `ObjectExt` de glib réexporté par gtk4 et celui réexporté par
        // gstreamer, tous deux en portée dans ce fichier.
        stack.connect_visible_child_name_notify(move |stack| {
            if stack.visible_child_name().as_deref() == Some("explorer")
                && !ui.explorer_warned.get()
            {
                stack.set_visible_child_name("abonnements");
                show_explorer_warning(&ui, &window, stack);
            }
        });
    }

    // Ouverture du nœud (async), puis abonnement aux mises à jour du catalogue :
    // le rafraîchissement est réactif (parité macOS/Windows), plus de bouton.
    {
        let ui = ui.clone();
        let status = status.clone();
        let subs_list = subs_list.clone();
        let explorer_list = explorer_list.clone();
        let search_entry = search_entry.clone();
        glib::spawn_future_local(async move {
            match open_node(&ui.rt).await {
                Ok(node) => {
                    status.set_text(&format!("nœud en ligne — {}", node.peer_id()));
                    let mut events = node.subscribe_catalog();
                    let mut seed_events = node.subscribe_seed();
                    *ui.node.borrow_mut() = Some(node);
                    // Les primitives tokio::sync fonctionnent sur l'exécuteur
                    // glib : la boucle vit sur le thread GTK et peut toucher
                    // les widgets directement. Un abonné en retard (Lagged) a
                    // seulement raté des tics : on rafraîchit quand même.
                    use tokio::sync::broadcast::error::RecvError;

                    // Boucle de seed proactif (lot c) : même patron que la
                    // boucle catalogue ci-dessous, sur un canal séparé (une
                    // publication seedée/purgée n'implique pas un changement
                    // de catalogue, et réciproquement).
                    {
                        let ui = ui.clone();
                        let status = status.clone();
                        let subs_list = subs_list.clone();
                        let explorer_list = explorer_list.clone();
                        let search_entry = search_entry.clone();
                        glib::spawn_future_local(async move {
                            while let Ok(()) | Err(RecvError::Lagged(_)) = seed_events.recv().await
                            {
                                refresh_lists(
                                    &ui,
                                    &status,
                                    &subs_list,
                                    &explorer_list,
                                    &search_entry,
                                );
                            }
                        });
                    }

                    while let Ok(()) | Err(RecvError::Lagged(_)) = events.recv().await {
                        refresh_lists(&ui, &status, &subs_list, &explorer_list, &search_entry);
                    }
                }
                Err(e) => status.set_text(&format!("erreur d'ouverture : {e}")),
            }
        });
    }

    // Réglages de seed : ouvre une popup dédiée (quota + usage courant).
    {
        let ui = ui.clone();
        let window = window.clone();
        seed_settings_btn.connect_clicked(move |_| {
            open_seed_settings(&ui, &window);
        });
    }

    // Réglages : liste des channels bloqués localement (tâche d/6) — popup
    // dédiée listant les PeerIds bloqués (tronqués) + bouton Débloquer.
    {
        let ui = ui.clone();
        let window = window.clone();
        blocked_channels_btn.connect_clicked(move |_| {
            open_blocked_channels(&ui, &window);
        });
    }

    // Recherche locale (titres/tags) : la logique vit dans le core, la vue ne
    // fait que réafficher les listes filtrées à chaque frappe.
    {
        let ui = ui.clone();
        let status = status.clone();
        let subs_list = subs_list.clone();
        let explorer_list = explorer_list.clone();
        search_entry.connect_changed(move |entry| {
            refresh_lists(&ui, &status, &subs_list, &explorer_list, entry);
        });
    }

    // Connexion à un pair (le catalogue se rafraîchit tout seul ensuite).
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

    // Aperçu par lien (`channel_link::parse`, tolérant : lien ou PeerId nu) —
    // résout SANS abonner, ouvre la feuille d'aperçu en cas de succès. Le
    // fetch réseau (`resolve_channel`) tourne sur le runtime tokio, jamais sur
    // le thread GTK (voir `resolve_channel_inner`).
    {
        let ui = ui.clone();
        let window = window.clone();
        let status = status.clone();
        let subs_list = subs_list.clone();
        let explorer_list = explorer_list.clone();
        let search_entry = search_entry.clone();
        let channel_entry = channel_entry.clone();
        let link_msg = link_msg.clone();
        let preview_link_btn = preview_link_btn.clone();
        let preview_spinner = preview_spinner.clone();
        preview_link_btn.clone().connect_clicked(move |_| {
            let text = channel_entry.text().to_string();
            let peer = match channel_link::parse(&text) {
                Ok(peer) => peer,
                Err(_) => {
                    link_msg.set_text("lien ou PeerId invalide");
                    return;
                }
            };
            link_msg.set_text("");
            let Some(node) = ui.node.borrow().clone() else {
                status.set_text("nœud pas encore prêt");
                return;
            };
            let rt = ui.rt.clone();
            let ui = ui.clone();
            let window = window.clone();
            let status = status.clone();
            let subs_list = subs_list.clone();
            let explorer_list = explorer_list.clone();
            let search_entry = search_entry.clone();
            let channel_entry = channel_entry.clone();
            let link_msg = link_msg.clone();
            let preview_link_btn = preview_link_btn.clone();
            let preview_spinner = preview_spinner.clone();
            preview_link_btn.set_sensitive(false);
            preview_spinner.set_spinning(true);
            glib::spawn_future_local(async move {
                let (tx, rx) = tokio::sync::oneshot::channel();
                rt.spawn(async move {
                    let _ = tx.send(resolve_channel_inner(&node, peer).await);
                });
                // `rt.spawn` ne fait que PLANIFIER la tâche — le bouton doit
                // rester désactivé et le spinner tourner jusqu'à ce que
                // `rx.await` résolve réellement la réponse réseau, pas juste
                // après l'avoir programmée.
                let result = rx.await;
                preview_link_btn.set_sensitive(true);
                preview_spinner.set_spinning(false);
                match result {
                    Ok(Ok(preview)) => {
                        channel_entry.set_text("");
                        open_channel_preview(
                            &ui,
                            &window,
                            &status,
                            &subs_list,
                            &explorer_list,
                            &search_entry,
                            preview,
                        );
                    }
                    Ok(Err(e)) => link_msg.set_text(&describe_preview_error(&e)),
                    Err(_) => link_msg.set_text("tâche annulée"),
                }
            });
        });
    }

    // Copie du lien partageable de mon propre channel.
    {
        let ui = ui.clone();
        let status = status.clone();
        copy_link_btn.connect_clicked(move |btn| {
            let Some(node) = ui.node.borrow().clone() else {
                status.set_text("nœud pas encore prêt");
                return;
            };
            let link = channel_link::format(&node.peer_id());
            btn.clipboard().set_text(&link);
            status.set_text("lien copié");
        });
    }

    window.present();
}

/// Affiche l'avertissement Explorer ; sur « Continuer », marque la session
/// comme avertie et bascule vers l'onglet Explorer.
fn show_explorer_warning(ui: &Rc<Ui>, window: &ApplicationWindow, stack: &Stack) {
    let dialog = MessageDialog::builder()
        .transient_for(window)
        .modal(true)
        .text("Explorer")
        .secondary_text(EXPLORER_WARNING)
        .buttons(ButtonsType::None)
        .build();
    dialog.add_button("Annuler", ResponseType::Cancel);
    dialog.add_button("Continuer", ResponseType::Accept);
    let ui = ui.clone();
    let stack = stack.clone();
    dialog.connect_response(move |dialog, resp| {
        if resp == ResponseType::Accept {
            ui.explorer_warned.set(true);
            stack.set_visible_child_name("explorer");
        }
        dialog.close();
    });
    dialog.present();
}

/// Reconstruit les deux listes (Abonnements/Explorer) : catalogue restreint
/// aux émetteurs souscrits ou catalogue complet (un en-tête par créateur, avec
/// bouton s'abonner/se désabonner), ou résultats de la recherche locale si la
/// recherche est non vide.
fn refresh_lists(
    ui: &Rc<Ui>,
    status: &Label,
    subs_list: &ListBox,
    explorer_list: &ListBox,
    search_entry: &Entry,
) {
    while let Some(child) = subs_list.first_child() {
        subs_list.remove(&child);
    }
    while let Some(child) = explorer_list.first_child() {
        explorer_list.remove(&child);
    }
    let Some(node) = ui.node.borrow().clone() else {
        return;
    };
    let subs: HashSet<PeerId> = node.subscriptions().into_iter().collect();
    let query = search_entry.text();
    let query = query.trim();
    if !query.is_empty() {
        let hits = node.search(query);
        status.set_text(&format!("recherche : {} résultat(s)", hits.len()));
        for hit in &hits {
            // Pas de pin dans les résultats de recherche (même choix que le
            // catalogue Explorer) : pas de contexte d'abonnement fiable ici.
            explorer_list.append(&content_row(
                ui,
                status,
                subs_list,
                explorer_list,
                search_entry,
                &hit.title,
                &hit.tags,
                &hit.cid.to_string(),
                None,
            ));
            if subs.contains(&hit.issuer) {
                subs_list.append(&content_row(
                    ui,
                    status,
                    subs_list,
                    explorer_list,
                    search_entry,
                    &hit.title,
                    &hit.tags,
                    &hit.cid.to_string(),
                    None,
                ));
            }
        }
        return;
    }
    let entries = node.catalog_entries();
    status.set_text(&format!("catalogue : {} créateur(s)", entries.len()));
    for entry in &entries {
        let (seeded, total, _pinned) = node.seed_coverage(&entry.cids);
        explorer_list.append(&channel_header_row(
            ui,
            status,
            subs_list,
            explorer_list,
            search_entry,
            entry,
            subs.contains(&entry.issuer),
            seeded,
            total,
            true, // can_block : Explorer uniquement, voir la doc de la fonction
        ));
        for item in &entry.items {
            // Pas de bouton de pin côté Explorer — épingler un contenu hors
            // abonnement n'a pas de sens dans cette UI (même décision que les
            // jumeaux macOS/Windows).
            explorer_list.append(&content_row(
                ui,
                status,
                subs_list,
                explorer_list,
                search_entry,
                &item.title,
                &item.tags,
                &item.cid.to_string(),
                None,
            ));
        }
    }
    let subscribed_entries = node.catalog_subscribed();
    for entry in &subscribed_entries {
        let (seeded, total, pinned) = node.seed_coverage(&entry.cids);
        subs_list.append(&channel_header_row(
            ui,
            status,
            subs_list,
            explorer_list,
            search_entry,
            entry,
            true,
            seeded,
            total,
            false, // can_block : un bloqué ne peut être souscrit, donc absent d'ici de toute façon
        ));
        for item in &entry.items {
            subs_list.append(&content_row(
                ui,
                status,
                subs_list,
                explorer_list,
                search_entry,
                &item.title,
                &item.tags,
                &item.cid.to_string(),
                Some(pinned.contains(&item.cid)),
            ));
        }
    }
}

/// En-tête d'un émetteur : nom du channel (ou PeerId tronqué) + seq + bouton
/// s'abonner/se désabonner (disponible dans les deux vues, au niveau
/// en-tête — pas par ligne de contenu).
#[allow(clippy::too_many_arguments)]
fn channel_header_row(
    ui: &Rc<Ui>,
    status: &Label,
    subs_list: &ListBox,
    explorer_list: &ListBox,
    search_entry: &Entry,
    entry: &CatalogEntry,
    subscribed: bool,
    seeded: u64,
    total: u64,
    can_block: bool,
) -> GtkBox {
    let row = GtkBox::new(Orientation::Horizontal, 8);
    let text = GtkBox::new(Orientation::Vertical, 2);
    let name = if entry.channel.name.is_empty() {
        truncate_peer_id(&entry.issuer.to_string())
    } else {
        entry.channel.name.clone()
    };
    let mut header_text = format!("{name} — seq {}", entry.seq);
    // État du seed proactif (lot c) — rien à afficher pour un feed vide.
    if total > 0 {
        if seeded == total {
            header_text.push_str(" · à jour");
        } else {
            header_text.push_str(&format!(" · seed en cours ({seeded}/{total})"));
        }
    }
    let header = Label::new(Some(&header_text));
    header.set_xalign(0.0);
    header.add_css_class("heading");
    text.append(&header);
    text.set_hexpand(true);
    row.append(&text);

    {
        let btn = Button::with_label(if subscribed {
            "Se désabonner"
        } else {
            "S'abonner"
        });
        let issuer = entry.issuer;
        let ui = ui.clone();
        let status = status.clone();
        let subs_list = subs_list.clone();
        let explorer_list = explorer_list.clone();
        let search_entry = search_entry.clone();
        btn.connect_clicked(move |_| {
            let Some(node) = ui.node.borrow().clone() else {
                status.set_text("nœud pas encore prêt");
                return;
            };
            let rt = ui.rt.clone();
            let ui = ui.clone();
            let status = status.clone();
            let subs_list = subs_list.clone();
            let explorer_list = explorer_list.clone();
            let search_entry = search_entry.clone();
            glib::spawn_future_local(async move {
                let (tx, rx) = tokio::sync::oneshot::channel();
                rt.spawn(async move {
                    let res = if subscribed {
                        unsubscribe_inner(&node, issuer).await
                    } else {
                        subscribe_inner(&node, issuer).await
                    };
                    let _ = tx.send(res);
                });
                match rx.await {
                    Ok(Ok(())) => {
                        status.set_text(if subscribed { "désabonné" } else { "abonné" });
                        refresh_lists(&ui, &status, &subs_list, &explorer_list, &search_entry);
                    }
                    Ok(Err(e)) => status.set_text(&describe_core_error(&e, "abonnement")),
                    Err(_) => status.set_text("tâche annulée"),
                }
            });
        });
        row.append(&btn);
    }

    // Blocage local de channel (tâche d/6) — Explorer uniquement (un channel
    // souscrit ne peut pas être bloqué sans se désabonner d'abord, et un
    // bloqué ne peut de toute façon pas apparaître dans Abonnements).
    if can_block {
        let block_btn = Button::with_label("Bloquer");
        let issuer = entry.issuer;
        let name = name.clone();
        let ui = ui.clone();
        let status = status.clone();
        let subs_list = subs_list.clone();
        let explorer_list = explorer_list.clone();
        let search_entry = search_entry.clone();
        block_btn.connect_clicked(move |_| {
            let dialog = MessageDialog::builder()
                .modal(true)
                .text("Bloquer ce channel ?")
                .secondary_text(format!(
                    "« {name} » disparaîtra du catalogue. Réversible depuis « Channels bloqués »."
                ))
                .buttons(ButtonsType::None)
                .build();
            if let Some(window) = ui.window.borrow().as_ref() {
                dialog.set_transient_for(Some(window));
            }
            dialog.add_button("Annuler", ResponseType::Cancel);
            dialog.add_button("Bloquer", ResponseType::Accept);
            let ui = ui.clone();
            let status = status.clone();
            let subs_list = subs_list.clone();
            let explorer_list = explorer_list.clone();
            let search_entry = search_entry.clone();
            dialog.connect_response(move |dialog, resp| {
                dialog.close();
                if resp != ResponseType::Accept {
                    return;
                }
                let Some(node) = ui.node.borrow().clone() else {
                    status.set_text("nœud pas encore prêt");
                    return;
                };
                let rt = ui.rt.clone();
                let ui = ui.clone();
                let status = status.clone();
                let subs_list = subs_list.clone();
                let explorer_list = explorer_list.clone();
                let search_entry = search_entry.clone();
                glib::spawn_future_local(async move {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    rt.spawn(async move {
                        let _ = tx.send(block_channel_inner(&node, issuer).await);
                    });
                    match rx.await {
                        Ok(Ok(())) => {
                            status.set_text("channel bloqué");
                            refresh_lists(&ui, &status, &subs_list, &explorer_list, &search_entry);
                        }
                        Ok(Err(e)) => status.set_text(&describe_core_error(&e, "blocage")),
                        Err(_) => status.set_text("tâche annulée"),
                    }
                });
            });
            dialog.present();
        });
        row.append(&block_btn);
    }

    row
}

/// Tronque un PeerId affiché (les 8 premiers + les 4 derniers caractères) —
/// PeerId s'affiche en base58, ASCII pur, le découpage par octets est sûr.
fn truncate_peer_id(id: &str) -> String {
    if id.chars().count() <= 14 {
        return id.to_string();
    }
    format!("{}…{}", &id[..8], &id[id.len() - 4..])
}

/// Message d'erreur distinguant un refus de modération (blocage délibéré) des
/// erreurs réseau/techniques.
fn describe_core_error(e: &CoreError, context: &str) -> String {
    match e {
        CoreError::Moderated(_) => "bloqué par la modération : refus délibéré du nœud".to_string(),
        other => format!("{context} : {other}"),
    }
}

/// Message d'erreur pour `resolve_channel` (tâche 3) — jamais refusé par la
/// modération (voir doc `resolve_channel`) ; seul un émetteur sans fournisseur
/// (aucun résultat DHT ni catalogue) est attendu ici. `InvalidInput` est géré
/// en amont par `channel_link::parse` (voir l'appelant).
fn describe_preview_error(e: &CoreError) -> String {
    match e {
        CoreError::NoProviders(_) => "channel introuvable pour l'instant".to_string(),
        other => format!("aperçu : {other}"),
    }
}

/// Une ligne de contenu : titre (ou CID si sans titre) + tags + bouton de pin
/// (Abonnements uniquement, quand `pinned` est fourni) + bouton « Lire ».
#[allow(clippy::too_many_arguments)]
fn content_row(
    ui: &Rc<Ui>,
    status: &Label,
    subs_list: &ListBox,
    explorer_list: &ListBox,
    search_entry: &Entry,
    title: &str,
    tags: &[String],
    cid: &str,
    pinned: Option<bool>,
) -> GtkBox {
    let row = GtkBox::new(Orientation::Horizontal, 8);
    let text = GtkBox::new(Orientation::Vertical, 2);
    let label = Label::new(Some(if title.is_empty() { cid } else { title }));
    label.set_xalign(0.0);
    text.append(&label);
    if !tags.is_empty() {
        let tags_label = Label::new(Some(&tags.join(" · ")));
        tags_label.set_xalign(0.0);
        tags_label.add_css_class("dim-label");
        text.append(&tags_label);
    }
    text.set_hexpand(true);
    row.append(&text);

    if let Some(is_pinned) = pinned {
        let pin_btn = Button::with_label(if is_pinned { "Oublier" } else { "Garder" });
        let ui = ui.clone();
        let status = status.clone();
        let subs_list = subs_list.clone();
        let explorer_list = explorer_list.clone();
        let search_entry = search_entry.clone();
        let cid_text = cid.to_string();
        pin_btn.connect_clicked(move |_| {
            let Some(node) = ui.node.borrow().clone() else {
                status.set_text("nœud pas encore prêt");
                return;
            };
            let Ok(manifest) = cid_text.parse::<Cid>() else {
                status.set_text("CID invalide");
                return;
            };
            let rt = ui.rt.clone();
            let ui = ui.clone();
            let status = status.clone();
            let subs_list = subs_list.clone();
            let explorer_list = explorer_list.clone();
            let search_entry = search_entry.clone();
            glib::spawn_future_local(async move {
                let (tx, rx) = tokio::sync::oneshot::channel();
                rt.spawn(async move {
                    let res = if is_pinned {
                        unpin_inner(&node, manifest).await
                    } else {
                        pin_inner(&node, manifest).await
                    };
                    let _ = tx.send(res);
                });
                match rx.await {
                    Ok(Ok(())) => {
                        refresh_lists(&ui, &status, &subs_list, &explorer_list, &search_entry);
                    }
                    Ok(Err(e)) => status.set_text(&describe_core_error(&e, "épinglage")),
                    Err(_) => status.set_text("tâche annulée"),
                }
            });
        });
        row.append(&pin_btn);
    }

    let play = Button::with_label("Lire");
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
        // Arrête la lecture précédente et supprime son répertoire (pas
        // d'accumulation de segments dans le tmp au fil des lectures).
        if let Some(old) = ui.player.borrow_mut().take() {
            let _ = old.set_state(gstreamer::State::Null);
        }
        if let Some(old_dir) = ui.current_play_dir.borrow_mut().take() {
            let _ = std::fs::remove_dir_all(old_dir);
        }
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
                        *ui.player.borrow_mut() = Some(player);
                        *ui.current_play_dir.borrow_mut() =
                            playlist.parent().map(Path::to_path_buf);
                        status.set_text("lecture en cours");
                    }
                    Err(e) => status.set_text(&format!("lecture : {e}")),
                },
                // Refus de modération (checkpoint #2) : blocage délibéré,
                // distinct d'une panne réseau.
                Ok(Err(e)) => status.set_text(&describe_core_error(&e, "récupération")),
                Err(_) => status.set_text("tâche annulée"),
            }
        });
    });
    row
}

/// Popup de réglages de seed (lot c) : quota (Go) + usage courant. Fenêtre
/// dédiée plutôt qu'un dialogue de la fenêtre principale — reste cohérente
/// avec la vue unique existante sans y ajouter de complexité permanente.
fn open_seed_settings(ui: &Rc<Ui>, parent: &ApplicationWindow) {
    let Some(node) = ui.node.borrow().clone() else {
        return;
    };
    let (used, quota) = node.storage_stats();

    let win = gtk::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title("Réglages de seed")
        .default_width(320)
        .build();

    let content = GtkBox::new(Orientation::Vertical, 8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);

    let title = Label::new(Some("Quota de seeding"));
    title.set_xalign(0.0);
    title.add_css_class("heading");

    let quota_row = GtkBox::new(Orientation::Horizontal, 8);
    let quota_entry = Entry::builder()
        .placeholder_text("Go")
        .text(format!("{:.1}", quota as f64 / 1_000_000_000.0))
        .build();
    let save_btn = Button::with_label("Enregistrer");
    quota_row.append(&quota_entry);
    quota_row.append(&save_btn);

    let stats_label = Label::new(Some(&storage_stats_text(used, quota)));
    stats_label.set_xalign(0.0);
    stats_label.add_css_class("dim-label");

    let msg_label = Label::new(None);
    msg_label.set_xalign(0.0);

    content.append(&title);
    content.append(&quota_row);
    content.append(&stats_label);
    content.append(&msg_label);
    win.set_child(Some(&content));

    let ui = ui.clone();
    save_btn.connect_clicked(move |_| {
        let Ok(gb) = quota_entry.text().replace(',', ".").parse::<f64>() else {
            msg_label.set_text("valeur invalide");
            return;
        };
        let bytes = (gb.max(0.0) * 1_000_000_000.0) as u64;
        let Some(node) = ui.node.borrow().clone() else {
            return;
        };
        let rt = ui.rt.clone();
        let ui = ui.clone();
        let stats_label = stats_label.clone();
        let msg_label = msg_label.clone();
        glib::spawn_future_local(async move {
            let (tx, rx) = tokio::sync::oneshot::channel();
            rt.spawn(async move {
                let _ = tx.send(set_seed_quota_inner(&node, bytes).await);
            });
            match rx.await {
                Ok(Ok(())) => {
                    msg_label.set_text("quota mis à jour");
                    if let Some(node) = ui.node.borrow().clone() {
                        let (used, quota) = node.storage_stats();
                        stats_label.set_text(&storage_stats_text(used, quota));
                    }
                }
                Ok(Err(e)) => msg_label.set_text(&format!("quota : {e}")),
                Err(_) => msg_label.set_text("tâche annulée"),
            }
        });
    });

    win.present();
}

/// Popup des channels bloqués localement (tâche d/6) : liste (PeerId tronqué
/// — le nom n'est plus au catalogue après purge) + bouton Débloquer par
/// ligne. Même patron de fenêtre dédiée que `open_seed_settings`.
fn open_blocked_channels(ui: &Rc<Ui>, parent: &ApplicationWindow) {
    if ui.node.borrow().is_none() {
        return;
    }

    let win = gtk::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title("Channels bloqués")
        .default_width(320)
        .build();

    let content = GtkBox::new(Orientation::Vertical, 8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);

    let title = Label::new(Some("Channels bloqués"));
    title.set_xalign(0.0);
    title.add_css_class("heading");

    let list = ListBox::new();
    let scroller = ScrolledWindow::builder()
        .child(&list)
        .min_content_height(160)
        .build();

    content.append(&title);
    content.append(&scroller);
    win.set_child(Some(&content));

    populate_blocked_channels(ui, &win, &list);

    win.present();
}

/// (Re)construit la liste des channels bloqués dans la popup — appelée à
/// l'ouverture et après chaque déblocage réussi.
fn populate_blocked_channels(ui: &Rc<Ui>, win: &gtk::Window, list: &ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let Some(node) = ui.node.borrow().clone() else {
        return;
    };
    let blocked = node.blocked_channels();
    if blocked.is_empty() {
        let empty = Label::new(Some("aucun channel bloqué"));
        empty.set_xalign(0.0);
        empty.add_css_class("dim-label");
        list.append(&empty);
        return;
    }
    for issuer in blocked {
        let row = GtkBox::new(Orientation::Horizontal, 8);
        let label = Label::new(Some(&truncate_peer_id(&issuer.to_string())));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        row.append(&label);

        let unblock_btn = Button::with_label("Débloquer");
        let ui = ui.clone();
        let win = win.clone();
        let list_for_closure = list.clone();
        unblock_btn.connect_clicked(move |_| {
            let Some(node) = ui.node.borrow().clone() else {
                return;
            };
            let rt = ui.rt.clone();
            let ui = ui.clone();
            let win = win.clone();
            let list = list_for_closure.clone();
            glib::spawn_future_local(async move {
                let (tx, rx) = tokio::sync::oneshot::channel();
                rt.spawn(async move {
                    let _ = tx.send(unblock_channel_inner(&node, issuer).await);
                });
                if let Ok(Ok(())) = rx.await {
                    populate_blocked_channels(&ui, &win, &list);
                }
            });
        });
        row.append(&unblock_btn);
        list.append(&row);
    }
}

/// Feuille d'aperçu d'un channel résolu par lien/PeerId (tâche 3) — même
/// patron de fenêtre dédiée que `open_seed_settings`/`open_blocked_channels`.
/// En-tête (nom/description, avatar si présent — sinon rien, pas de
/// placeholder cassé) + liste des publications + pied selon
/// abonné/bloqué : bouton S'abonner/Se désabonner, ou libellé "Channel
/// bloqué" sans bouton. Ferme la fenêtre après une action réussie — les
/// listes se rafraîchissent réactivement via le `CatalogListener` du core, le
/// `refresh_lists` explicite ici donne un retour immédiat (même choix que le
/// reste de ce fichier).
#[allow(clippy::too_many_arguments)]
fn open_channel_preview(
    ui: &Rc<Ui>,
    parent: &ApplicationWindow,
    status: &Label,
    subs_list: &ListBox,
    explorer_list: &ListBox,
    search_entry: &Entry,
    preview: ChannelPreview,
) {
    let win = gtk::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title("Aperçu du channel")
        .default_width(360)
        .build();

    let content = GtkBox::new(Orientation::Vertical, 8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);

    let name = if preview.channel.name.is_empty() {
        truncate_peer_id(&preview.issuer.to_string())
    } else {
        preview.channel.name.clone()
    };
    let name_label = Label::new(Some(&name));
    name_label.set_xalign(0.0);
    name_label.add_css_class("heading");
    content.append(&name_label);

    if !preview.channel.description.is_empty() {
        let desc_label = Label::new(Some(&preview.channel.description));
        desc_label.set_xalign(0.0);
        desc_label.set_wrap(true);
        content.append(&desc_label);
    }

    // Avatar si présent — sinon rien du tout (pas de placeholder cassé) :
    // c'est un CID, pas d'image chargée ici (même choix que les jumeaux
    // macOS/Windows, qui n'affichent qu'une mention texte).
    if let Some(avatar_cid) = &preview.channel.avatar_cid {
        if !avatar_cid.is_empty() {
            let avatar_label = Label::new(Some(&format!("avatar : {avatar_cid}")));
            avatar_label.set_xalign(0.0);
            avatar_label.add_css_class("dim-label");
            content.append(&avatar_label);
        }
    }

    let items_title = Label::new(Some("Publications"));
    items_title.set_xalign(0.0);
    items_title.add_css_class("heading");
    content.append(&items_title);

    if preview.items.is_empty() {
        let empty = Label::new(Some("aucune publication"));
        empty.set_xalign(0.0);
        empty.add_css_class("dim-label");
        content.append(&empty);
    } else {
        let items_list = ListBox::new();
        for item in &preview.items {
            let row = GtkBox::new(Orientation::Vertical, 2);
            let title_text = if item.title.is_empty() {
                item.cid.to_string()
            } else {
                item.title.clone()
            };
            let title_label = Label::new(Some(&title_text));
            title_label.set_xalign(0.0);
            row.append(&title_label);
            if !item.tags.is_empty() {
                let tags_label = Label::new(Some(&item.tags.join(" · ")));
                tags_label.set_xalign(0.0);
                tags_label.add_css_class("dim-label");
                row.append(&tags_label);
            }
            items_list.append(&row);
        }
        let scroller = ScrolledWindow::builder()
            .child(&items_list)
            .min_content_height(160)
            .build();
        content.append(&scroller);
    }

    // Pied : bouton selon abonné/bloqué — présentation pure, aucun état
    // recalculé côté front (`subscribed`/`blocked` viennent de l'aperçu).
    if preview.blocked {
        let blocked_label = Label::new(Some("Channel bloqué"));
        blocked_label.set_xalign(0.0);
        content.append(&blocked_label);
    } else {
        let action_btn = Button::with_label(if preview.subscribed {
            "Se désabonner"
        } else {
            "S'abonner"
        });
        let issuer = preview.issuer;
        let subscribed = preview.subscribed;
        let ui = ui.clone();
        let win = win.clone();
        let status = status.clone();
        let subs_list = subs_list.clone();
        let explorer_list = explorer_list.clone();
        let search_entry = search_entry.clone();
        action_btn.connect_clicked(move |_| {
            let Some(node) = ui.node.borrow().clone() else {
                status.set_text("nœud pas encore prêt");
                return;
            };
            let rt = ui.rt.clone();
            let ui = ui.clone();
            let win = win.clone();
            let status = status.clone();
            let subs_list = subs_list.clone();
            let explorer_list = explorer_list.clone();
            let search_entry = search_entry.clone();
            glib::spawn_future_local(async move {
                let (tx, rx) = tokio::sync::oneshot::channel();
                rt.spawn(async move {
                    let res = if subscribed {
                        unsubscribe_inner(&node, issuer).await
                    } else {
                        subscribe_inner(&node, issuer).await
                    };
                    let _ = tx.send(res);
                });
                match rx.await {
                    Ok(Ok(())) => {
                        status.set_text(if subscribed { "désabonné" } else { "abonné" });
                        refresh_lists(&ui, &status, &subs_list, &explorer_list, &search_entry);
                        win.close();
                    }
                    Ok(Err(e)) => status.set_text(&describe_core_error(&e, "abonnement")),
                    Err(_) => status.set_text("tâche annulée"),
                }
            });
        });
        content.append(&action_btn);
    }

    win.set_child(Some(&content));
    win.present();
}

/// Affichage humain de `(octets_utilisés, quota_octets)`.
fn storage_stats_text(used: u64, quota: u64) -> String {
    format!(
        "Utilisé : {:.1} Go / {:.1} Go",
        used as f64 / 1_000_000_000.0,
        quota as f64 / 1_000_000_000.0
    )
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

async fn fetch_inner(node: &Node, manifest: Cid) -> Result<PathBuf, CoreError> {
    let out = play_root().join(manifest.to_string());
    node.fetch_hls(manifest, &out).await
}

/// S'abonne à un émetteur — appel sync du core (écriture disque + tâche de
/// fond `tokio::spawn`), donc exécuté sur le runtime tokio (jamais sur le
/// thread principal GTK).
async fn subscribe_inner(node: &Node, issuer: PeerId) -> Result<(), CoreError> {
    node.subscribe(issuer)
}

/// Se désabonne d'un émetteur — même contrainte que `subscribe_inner`.
async fn unsubscribe_inner(node: &Node, issuer: PeerId) -> Result<(), CoreError> {
    node.unsubscribe(issuer)
}

/// Bloque un channel localement (tâche d/6) — appel réellement async côté
/// core (désabonne + purge le catalogue), exécuté sur le runtime tokio.
async fn block_channel_inner(node: &Node, issuer: PeerId) -> Result<(), CoreError> {
    node.block_channel(issuer).await
}

/// Résout un aperçu de channel (tâche 3) — catalogue local d'abord, sinon
/// DHT (appel réseau potentiellement lent) : exécuté sur le runtime tokio,
/// jamais sur le thread principal GTK.
async fn resolve_channel_inner(node: &Node, issuer: PeerId) -> Result<ChannelPreview, CoreError> {
    node.resolve_channel(issuer).await
}

/// Débloque un channel bloqué localement — appel sync du core, même
/// contrainte que `unsubscribe_inner`.
async fn unblock_channel_inner(node: &Node, issuer: PeerId) -> Result<(), CoreError> {
    node.unblock_channel(issuer)
}

/// Épingle un manifeste (écriture disque du `SeedIndex`) — même contrainte
/// que `subscribe_inner` : jamais sur le thread principal GTK.
async fn pin_inner(node: &Node, manifest_cid: Cid) -> Result<(), CoreError> {
    node.pin(manifest_cid)
}

/// Retire l'épinglage d'un manifeste — même contrainte que `pin_inner`.
async fn unpin_inner(node: &Node, manifest_cid: Cid) -> Result<(), CoreError> {
    node.unpin(manifest_cid)
}

/// Définit le quota de seed (persiste sur disque) — même contrainte que
/// `subscribe_inner`.
async fn set_seed_quota_inner(node: &Node, bytes: u64) -> Result<(), CoreError> {
    node.set_seed_quota(bytes)
}
