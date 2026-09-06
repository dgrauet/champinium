//! Liens partageables `champinium://…` : forme lisible d'un `PeerId`.
//!
//! - **channel** `champinium://channel/<peerid>` (spec channels §3 — bouton
//!   « copier le lien de mon channel ») ;
//! - **denylist** `champinium://denylist/<peerid>` (spec 2026-09-06) : partage
//!   d'un **éditeur** de liste de modération, et non d'une liste — la liste
//!   signée vit dans la DHT, sous la clé de son éditeur.
//!
//! Les deux `parse` sont tolérants (un `PeerId` nu et les espaces de bord sont
//! acceptés) mais **pas interchangeables** : un lien de channel collé là où on
//! attend une denylist est refusé plutôt que réinterprété — souscrire à un
//! éditeur de modération n'est pas s'abonner à un channel.

use crate::error::CoreError;
use crate::error::Result as CoreResult;
use libp2p::PeerId;
use std::str::FromStr;

const PREFIX: &str = "champinium://channel/";
const DENYLIST_PREFIX: &str = "champinium://denylist/";

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

/// Formate un lien d'éditeur de denylist à partir d'un `PeerId`.
pub fn denylist_link(peer: &PeerId) -> String {
    std::format!("{DENYLIST_PREFIX}{peer}")
}

/// Parse un lien d'éditeur de denylist OU un `PeerId` nu (espaces de bord
/// tolérés). Un lien de **channel** est explicitement refusé : les deux
/// espaces de nommage ne désignent pas la même chose, et accepter l'un pour
/// l'autre ferait souscrire à la modération d'une clé qu'on voulait
/// simplement suivre.
pub fn parse_denylist(s: &str) -> CoreResult<PeerId> {
    let trimmed = s.trim();
    if trimmed.starts_with(PREFIX) {
        return Err(CoreError::Identity(
            "lien de channel, pas de lien d'éditeur de denylist".into(),
        ));
    }
    let candidate = trimmed.strip_prefix(DENYLIST_PREFIX).unwrap_or(trimmed);
    PeerId::from_str(candidate)
        .map_err(|e| CoreError::Identity(format!("lien de denylist invalide: {e}")))
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

    #[test]
    fn denylist_link_roundtrip() {
        let peer = Keypair::generate_ed25519().public().to_peer_id();
        let link = denylist_link(&peer);
        assert!(link.starts_with("champinium://denylist/"));
        assert_eq!(parse_denylist(&link).unwrap(), peer);
        assert_eq!(
            parse_denylist(&peer.to_string()).unwrap(),
            peer,
            "PeerId nu accepté"
        );
        assert_eq!(
            parse_denylist(&format!("  {link}\n")).unwrap(),
            peer,
            "espaces tolérés"
        );
        assert!(
            parse_denylist(&format(&peer)).is_err(),
            "un lien de channel n'est pas un lien de denylist"
        );
        assert!(parse_denylist("champinium://denylist/pas-une-clé").is_err());
    }
}
