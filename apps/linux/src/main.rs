//! Champinium — front Linux natif GTK4 (Phase 4).
//!
//! Présentation UNIQUEMENT : toute la logique vit dans `champinium-core`, consommé
//! directement (Rust → Rust, pas de FFI). L'UI GTK4 + la lecture GStreamer sont
//! derrière la feature `gui` (libs système requises). Sans cette feature, le
//! binaire reste compilable partout (utile pour le build du workspace en CI).

#[cfg(feature = "gui")]
mod gui;

fn main() {
    #[cfg(feature = "gui")]
    gui::run();

    #[cfg(not(feature = "gui"))]
    eprintln!(
        "Champinium Linux — recompilez avec `--features gui` (GTK4 + GStreamer requis) \
         pour l'interface. Noyau v{}.",
        champinium_core::core_version()
    );
}
