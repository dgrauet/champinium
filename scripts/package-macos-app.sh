#!/usr/bin/env bash
# Assemble Champinium.app (bundle macOS) SANS compte développeur payant :
# signature ad-hoc (codesign -s -), pas de notarisation. Au premier lancement,
# Gatekeeper exige un clic droit → Ouvrir (documenté dans docs/packaging.md).
#
# Prérequis : `swift build -c release` réalisable (bindings préparés via
# `just macos-prepare` / la CI). Sortie : dist/Champinium-macos.zip
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP_DIR="$ROOT/dist/Champinium.app"
VERSION="$(sed -n 's/.*"\.": "\(.*\)".*/\1/p' "$ROOT/.release-please-manifest.json")"

cd "$ROOT/apps/macos"
swift build -c release

EXE="$ROOT/apps/macos/.build/release/Champinium"
DYLIB="$ROOT/target/release/libchampinium_core.dylib"

rm -rf "$ROOT/dist"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Frameworks" "$APP_DIR/Contents/Resources"

cp "$EXE" "$APP_DIR/Contents/MacOS/Champinium"
cp "$DYLIB" "$APP_DIR/Contents/Frameworks/libchampinium_core.dylib"

cat > "$APP_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key><string>Champinium</string>
    <key>CFBundleIdentifier</key><string>org.champinium.macos</string>
    <key>CFBundleName</key><string>Champinium</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>${VERSION}</string>
    <key>CFBundleVersion</key><string>${VERSION}</string>
    <key>LSMinimumSystemVersion</key><string>13.0</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# Rebase la lib native sur @rpath : le binaire SwiftPM la référence par le
# chemin absolu du poste de build, inutilisable ailleurs.
LINKED_PATH="$(otool -L "$APP_DIR/Contents/MacOS/Champinium" \
    | awk '/libchampinium_core\.dylib/ {print $1; exit}')"
install_name_tool -id "@rpath/libchampinium_core.dylib" \
    "$APP_DIR/Contents/Frameworks/libchampinium_core.dylib"
install_name_tool -change "$LINKED_PATH" "@rpath/libchampinium_core.dylib" \
    "$APP_DIR/Contents/MacOS/Champinium"
install_name_tool -add_rpath "@executable_path/../Frameworks" \
    "$APP_DIR/Contents/MacOS/Champinium"

# Signature AD-HOC (gratuite) : intègre l'app pour Gatekeeper local, sans
# identité Apple. install_name_tool a invalidé les signatures → --force.
codesign --force -s - "$APP_DIR/Contents/Frameworks/libchampinium_core.dylib"
codesign --force -s - "$APP_DIR"
codesign --verify --deep "$APP_DIR"

# Vérifie que l'app est auto-suffisante : plus aucune référence au chemin de
# build, la lib se résout via @rpath. (Pas de lancement ici : une app SwiftUI
# ne rend pas la main.)
otool -L "$APP_DIR/Contents/MacOS/Champinium" | grep -q "@rpath/libchampinium_core.dylib"
! otool -L "$APP_DIR/Contents/MacOS/Champinium" | grep -q "$ROOT/target"

ditto -c -k --keepParent "$APP_DIR" "$ROOT/dist/Champinium-macos.zip"
echo "OK: dist/Champinium-macos.zip (v${VERSION}, signé ad-hoc — non notarisé)"
