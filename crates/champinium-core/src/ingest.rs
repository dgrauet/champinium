//! Ingestion média : orchestration ffmpeg → segmentation HLS.
//!
//! `ffmpeg` segmente le média source en HLS. Chaque segment devient un bloc
//! adressé par CID (stocké + annoncé), et un **manifeste** (`champinium-hls/v1`)
//! mappe l'ordre/durée des segments à leurs CIDs. Le manifeste lui-même est un
//! bloc (son CID identifie le « contenu » à publier dans un feed).
//!
//! Le checkpoint de modération #1 s'applique sur le chemin réel d'ingestion :
//! chaque segment passe par `Node::add`, qui refuse tout CID matché.

use crate::error::{CoreError, Result as CoreResult};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Identifiant de schéma de manifeste HLS.
pub const HLS_SCHEMA: &str = "champinium-hls/v1";

/// Un segment HLS : CID du bloc + durée (secondes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HlsSegment {
    pub cid: String,
    pub duration: f32,
}

/// Manifeste HLS content-addressed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HlsManifest {
    pub schema: String,
    pub target_duration: f32,
    pub segments: Vec<HlsSegment>,
}

impl HlsManifest {
    /// Construit un manifeste.
    pub fn new(target_duration: f32, segments: Vec<HlsSegment>) -> Self {
        Self {
            schema: HLS_SCHEMA.to_string(),
            target_duration,
            segments,
        }
    }

    /// Sérialise en JSON.
    pub fn to_json(&self) -> CoreResult<String> {
        serde_json::to_string(self).map_err(|e| CoreError::Ingest(format!("json: {e}")))
    }

    /// Parse depuis JSON (et valide le schéma).
    pub fn from_json(bytes: &[u8]) -> CoreResult<Self> {
        let m: Self =
            serde_json::from_slice(bytes).map_err(|e| CoreError::Ingest(format!("json: {e}")))?;
        if m.schema != HLS_SCHEMA {
            return Err(CoreError::Ingest(format!("schéma inconnu: {}", m.schema)));
        }
        Ok(m)
    }

    /// Reconstruit un playlist `.m3u8` jouable, chaque segment pointant vers un
    /// fichier local nommé `<cid>.ts`.
    pub fn to_m3u8(&self) -> String {
        let target = self
            .target_duration
            .max(
                self.segments
                    .iter()
                    .map(|s| s.duration)
                    .fold(0.0_f32, f32::max),
            )
            .ceil() as u32;
        let mut s = String::new();
        s.push_str("#EXTM3U\n#EXT-X-VERSION:3\n");
        s.push_str(&format!("#EXT-X-TARGETDURATION:{target}\n"));
        s.push_str("#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:VOD\n");
        for seg in &self.segments {
            s.push_str(&format!("#EXTINF:{:.3},\n{}.ts\n", seg.duration, seg.cid));
        }
        s.push_str("#EXT-X-ENDLIST\n");
        s
    }
}

/// Lance ffmpeg pour segmenter `input` en HLS dans `out_dir`.
/// Renvoie le chemin du playlist généré.
pub async fn run_ffmpeg_hls(input: &Path, out_dir: &Path, hls_time: u32) -> CoreResult<PathBuf> {
    let playlist = out_dir.join("index.m3u8");
    let seg_pattern = out_dir.join("seg_%05d.ts");
    // Force des keyframes alignées sur les frontières de segment, sinon ffmpeg ne
    // peut pas découper avant la keyframe suivante (segments alignés sur le GOP).
    let keyframes = format!("expr:gte(t,n_forced*{hls_time})");
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(input)
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac"])
        .arg("-force_key_frames")
        .arg(&keyframes)
        .args(["-f", "hls", "-hls_time"])
        .arg(hls_time.to_string())
        .args(["-hls_playlist_type", "vod", "-hls_segment_filename"])
        .arg(&seg_pattern)
        .arg(&playlist)
        .status()
        .await
        .map_err(|e| CoreError::Ingest(format!("ffmpeg introuvable ou non exécutable: {e}")))?;
    if !status.success() {
        return Err(CoreError::Ingest(format!(
            "ffmpeg a échoué (code {:?})",
            status.code()
        )));
    }
    Ok(playlist)
}

/// Parse un playlist HLS : renvoie (target_duration, [(chemin segment, durée)]).
pub fn parse_playlist(m3u8: &str, dir: &Path) -> CoreResult<(f32, Vec<(PathBuf, f32)>)> {
    let mut target = 0.0_f32;
    let mut pending = 0.0_f32;
    let mut segments = Vec::new();
    for line in m3u8.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            target = rest.trim().parse().unwrap_or(0.0);
        } else if let Some(rest) = line.strip_prefix("#EXTINF:") {
            pending = rest
                .split(',')
                .next()
                .unwrap_or("0")
                .trim()
                .parse()
                .unwrap_or(0.0);
        } else if line.is_empty() || line.starts_with('#') {
            continue;
        } else {
            segments.push((dir.join(line), pending));
            pending = 0.0;
        }
    }
    if segments.is_empty() {
        return Err(CoreError::Ingest("playlist sans segment".into()));
    }
    Ok((target, segments))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_json_roundtrip_and_schema_check() {
        let m = HlsManifest::new(
            4.0,
            vec![HlsSegment {
                cid: "bafyfake".into(),
                duration: 3.5,
            }],
        );
        let json = m.to_json().unwrap();
        let back = HlsManifest::from_json(json.as_bytes()).unwrap();
        assert_eq!(back.segments.len(), 1);
        // Mauvais schéma rejeté.
        let bad = json.replace(HLS_SCHEMA, "autre/v9");
        assert!(HlsManifest::from_json(bad.as_bytes()).is_err());
    }

    #[test]
    fn parse_playlist_extracts_segments() {
        let m3u8 = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:4\n#EXTINF:3.500,\nseg_00000.ts\n#EXTINF:2.000,\nseg_00001.ts\n#EXT-X-ENDLIST\n";
        let (target, segs) = parse_playlist(m3u8, Path::new("/tmp/x")).unwrap();
        assert_eq!(target, 4.0);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].1, 3.5);
        assert!(segs[1].0.ends_with("seg_00001.ts"));
    }

    #[test]
    fn to_m3u8_has_endlist_and_segments() {
        let m = HlsManifest::new(
            4.0,
            vec![
                HlsSegment {
                    cid: "cidA".into(),
                    duration: 3.5,
                },
                HlsSegment {
                    cid: "cidB".into(),
                    duration: 1.2,
                },
            ],
        );
        let pl = m.to_m3u8();
        assert!(pl.contains("#EXTM3U"));
        assert!(pl.contains("cidA.ts"));
        assert!(pl.contains("#EXT-X-ENDLIST"));
    }
}
