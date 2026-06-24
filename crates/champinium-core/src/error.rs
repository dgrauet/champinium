//! Types d'erreur du noyau.

use thiserror::Error;

/// Erreur du noyau Champinium.
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),

    #[error("CID invalide: {0}")]
    Cid(#[from] cid::Error),

    #[error("intégrité: le bloc reçu ne correspond pas au CID demandé")]
    IntegrityMismatch,

    #[error("bloc introuvable: {0}")]
    BlockNotFound(String),

    #[error("aucun fournisseur trouvé pour {0}")]
    NoProviders(String),

    #[error("identité: {0}")]
    Identity(String),

    #[error("réseau: {0}")]
    Network(String),

    #[error("denylist: {0}")]
    Moderation(String),

    #[error("ingestion: {0}")]
    Ingest(String),

    #[error("contenu refusé par la modération: {0}")]
    Moderated(String),

    #[error("le noyau s'est arrêté")]
    Shutdown,
}

/// Résultat du noyau.
pub type Result<T> = std::result::Result<T, CoreError>;
