//! Chemins de données par défaut, par OS.
//!
//! L'identité Ed25519 et le `seq` de feed persistés DOIVENT vivre dans un
//! répertoire durable, pas dans un temporaire purgeable par l'OS (sinon un
//! nettoyage du tmp fait perdre le PeerId et régresse le `seq`, cassant le LWW
//! des catalogues pairs). Ce helper donne l'emplacement canonique par plateforme.

use std::path::PathBuf;

/// Répertoire de données durable de l'application (créé si nécessaire côté
/// appelant). Convention par OS :
/// - **Windows** : `%LOCALAPPDATA%\Champinium`
/// - **macOS** : `~/Library/Application Support/Champinium`
/// - **Linux/autres** : `$XDG_DATA_HOME/champinium` sinon `~/.local/share/champinium`
///
/// Repli sur le répertoire courant si aucune variable d'environnement d'accueil
/// n'est disponible (cas dégradé, ne devrait pas arriver en usage normal).
pub fn default_data_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(local).join("Champinium");
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("Champinium");
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            return PathBuf::from(xdg).join("champinium");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("champinium");
        }
    }
    PathBuf::from("champinium-data")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_data_dir_ends_with_app_name() {
        let dir = default_data_dir();
        let last = dir.file_name().and_then(|s| s.to_str()).unwrap();
        assert!(
            last.eq_ignore_ascii_case("champinium") || last == "champinium-data",
            "répertoire inattendu: {dir:?}"
        );
    }

    #[test]
    fn default_data_dir_is_not_temp() {
        let dir = default_data_dir();
        let tmp = std::env::temp_dir();
        assert!(
            !dir.starts_with(&tmp),
            "les données durables ne doivent pas vivre dans {tmp:?}"
        );
    }
}
