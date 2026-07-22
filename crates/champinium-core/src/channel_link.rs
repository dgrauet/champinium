//! Lien de channel `champinium://channel/<peerid>` : forme partageable d'un
//! `PeerId` (spec channels §3 — bouton « copier le lien de mon channel »).
//! `parse` est tolérant : accepte aussi un `PeerId` nu, espaces de bord inclus.

use crate::error::CoreError;
use crate::error::Result as CoreResult;
use libp2p::PeerId;
use std::str::FromStr;

const PREFIX: &str = "champinium://channel/";

/// Formate un lien de channel à partir d'un `PeerId`.
pub fn format(peer: &PeerId) -> String {
    std::format!("{PREFIX}{peer}")
}

/// Parse un lien de channel OU un `PeerId` nu (espaces de bord tolérés).
pub fn parse(s: &str) -> CoreResult<PeerId> {
    let trimmed = s.trim();
    let candidate = trimmed.strip_prefix(PREFIX).unwrap_or(trimmed);
    PeerId::from_str(candidate)
        .map_err(|e| CoreError::Identity(format!("lien de channel invalide: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::identity::Keypair;

    #[test]
    fn roundtrip_and_tolerant_parse() {
        let peer = Keypair::generate_ed25519().public().to_peer_id();
        let link = format(&peer);
        assert!(link.starts_with("champinium://channel/"));
        assert_eq!(parse(&link).unwrap(), peer);
        assert_eq!(parse(&peer.to_string()).unwrap(), peer, "PeerId nu accepté");
        assert_eq!(
            parse(&format!("  {link}\n")).unwrap(),
            peer,
            "espaces tolérés"
        );
        assert!(parse("champinium://channel/pas-une-clé").is_err());
        assert!(parse("https://exemple.com/x").is_err());
    }
}
