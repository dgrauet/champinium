//! Champinium — front Linux natif GTK4 (SQUELETTE).
//!
//! Présentation UNIQUEMENT. Aucune logique métier : tout passe par champinium-core
//! (consommé directement, sans FFI, puisque le front Linux est en Rust).
//! Lecture média cible : GStreamer (hlsdemux). UI cible : GTK4 via gtk-rs.
fn main() {
    println!(
        "Champinium Linux (squelette GTK4) — noyau v{}",
        champinium_core::core_version()
    );
    println!("TODO Phase 4 : fenêtre GTK4 (catalogue + lecture GStreamer).");
}
