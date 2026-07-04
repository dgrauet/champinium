#!/usr/bin/env bash
# Tarball du front Linux GTK4 (palier gratuit : pas de Flatpak/AppImage encore).
# Le binaire dépend des bibliothèques SYSTÈME GTK4 + GStreamer (voir README
# embarqué). Prérequis : `cargo build --release -p champinium-linux --features gui`.
# Sortie : dist/Champinium-linux-x86_64.tar.gz
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION="$(sed -n 's/.*"\.": "\(.*\)".*/\1/p' "$ROOT/.release-please-manifest.json")"
PKG="$ROOT/dist/champinium-linux"

rm -rf "$ROOT/dist"
mkdir -p "$PKG"

cp "$ROOT/target/release/champinium-linux" "$PKG/champinium"

cat > "$PKG/champinium.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Champinium
Comment=Partage P2P décentralisé de contenu généré par IA
Exec=champinium
Terminal=false
Categories=AudioVideo;Network;
DESKTOP

cat > "$PKG/README" <<README
Champinium v${VERSION} — front Linux (GTK4)

Dépendances système (non embarquées) :
  Debian/Ubuntu : sudo apt install libgtk-4-1 gstreamer1.0-plugins-good \\
                  gstreamer1.0-plugins-bad gstreamer1.0-libav
  Fedora        : sudo dnf install gtk4 gstreamer1-plugins-good \\
                  gstreamer1-plugins-bad-free gstreamer1-libav

Lancement : ./champinium
Installation (optionnelle) :
  install -Dm755 champinium ~/.local/bin/champinium
  install -Dm644 champinium.desktop ~/.local/share/applications/champinium.desktop

Données du nœud (identité + blocs) : \$XDG_DATA_HOME/champinium
(ou ~/.local/share/champinium).
README

tar -C "$ROOT/dist" -czf "$ROOT/dist/Champinium-linux-x86_64.tar.gz" champinium-linux
echo "OK: dist/Champinium-linux-x86_64.tar.gz (v${VERSION})"
