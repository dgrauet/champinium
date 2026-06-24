# Champinium — front Linux (GTK4 / gtk-rs)

Front natif Linux. **Présentation uniquement** : toute la logique vit dans
`champinium-core`, consommé **directement** (Rust → Rust, pas de FFI).

- UI : GTK4 (gtk-rs)
- Lecture média : GStreamer (`playbin`)
- L'interface est derrière la feature **`gui`** (libs système requises), pour que
  `cargo build` du workspace reste vert sur les machines sans GTK/GStreamer.

## Build

Prérequis (Debian/Ubuntu) :

```sh
sudo apt install pkg-config libgtk-4-dev libgstreamer1.0-dev \
    gstreamer1.0-plugins-base gstreamer1.0-plugins-good
```

(macOS pour développement : `brew install pkg-config gtk4 gstreamer`.)

```sh
cargo run -p champinium-linux --features gui      # lance l'interface
cargo build -p champinium-linux                   # build « stub » sans GTK (CI)
```

## UI (Phase 4)

`gui.rs` : ouverture du nœud → `listen` → connexion à un pair → catalogue
reconstruit → bouton « Lire » → `fetch_hls` puis lecture **GStreamer**. Un runtime
tokio exécute les appels async du noyau ; les résultats reviennent sur le thread
GTK via `glib::spawn_future_local` + oneshot.

## Statut de vérification

- ✅ `cargo build -p champinium-linux` (sans feature) et le workspace : compilent.
- ⚠️ `--features gui` : **non compilé dans l'environnement de dev macOS** (ni
  `pkg-config` ni GTK4/GStreamer). Les versions de crates (gtk4 0.9, gstreamer
  0.23) résolvent ; à compiler/valider sur Linux (ou macOS avec les libs).

Seeding en arrière-plan (systemd user service) : à venir dans la Phase 4.
Packaging Phase 6 : Flatpak / AppImage / .deb.
