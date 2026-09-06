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
reconstruit → bouton « Lire » → `open_stream` ouvre une session de lecture
progressive servie par le noyau sur `127.0.0.1` (ADR 0009), dont l'URL est
passée directement à `playbin` (**GStreamer**), avec une ligne de progression
« segments : x/y » et `close_stream` à l'arrêt. Chaque entrée du catalogue
affiche un badge de provenance déclarée (« IA / Assisté IA / Capturé / Non
déclaré » + outils, ADR 0010) — pas de champ de saisie, la publication reste
CLI-only. Un runtime tokio exécute les appels async du noyau ; les résultats
reviennent sur le thread GTK via `glib::spawn_future_local` + oneshot.

Un volet **« Listes de modération »** (liste projet verrouillée, champ de
collage + « Suivre » pour un éditeur tiers, état « jamais récupérée » tant que
rien n'est en cache) couvre les denylists distribuées par le réseau (ADR 0011).

## Statut de vérification

- ✅ `cargo build -p champinium-linux` (sans feature) et le workspace : compilent.
- ⚠️ `--features gui` : **non compilé dans l'environnement de dev macOS** (ni
  `pkg-config` ni GTK4/GStreamer). Les versions de crates (gtk4 0.9, gstreamer
  0.23) résolvent ; à compiler/valider sur Linux (ou macOS avec les libs).

Seeding en arrière-plan (systemd user service) : à venir dans la Phase 4.
Packaging Phase 6 : Flatpak / AppImage / .deb.
