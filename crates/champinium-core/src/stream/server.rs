//! Serveur HTTP/1.1 minimal de la lecture progressive : `127.0.0.1`, port
//! éphémère, GET/HEAD seulement, `Range` à un intervalle, jeton d'URL de
//! 128 bits (un autre process local ne peut pas deviner la session).
//! Aucune liste de sessions n'est exposée.

use bytes::Bytes;
use futures::future::BoxFuture;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::header::{
    ACCEPT_RANGES, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE,
};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::task::{JoinHandle, JoinSet};

pub const STREAM_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Échec de résolution d'un segment, traduit en statut HTTP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegmentError {
    /// Refusé par la modération → 403.
    Forbidden,
    /// Index hors manifeste → 404.
    NotFound,
    /// Pas disponible dans le délai → 503 (le lecteur réessaie).
    Timeout,
    /// Autre → 500.
    Internal(String),
}

/// Ce que le serveur sert : la playlist et les segments par index. Le futur
/// de `segment` peut rester en attente le temps de la récupération P2P ; le
/// serveur borne l'attente à [`STREAM_REQUEST_TIMEOUT`].
pub trait SegmentSource: Send + Sync + 'static {
    fn playlist(&self) -> String;
    fn segment(&self, index: usize) -> BoxFuture<'static, Result<Vec<u8>, SegmentError>>;
}

pub struct LocalHttpServer {
    port: u16,
    token: String,
    accept_task: JoinHandle<()>,
}

impl LocalHttpServer {
    pub async fn start(source: Arc<dyn SegmentSource>) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let token = format!("{:032x}", rand::random::<u128>());
        let tok = token.clone();
        let accept_task = tokio::spawn(async move {
            // `JoinSet` : abandonné avec la tâche d'accept → toutes les
            // connexions en cours sont annulées à l'arrêt.
            let mut conns = JoinSet::new();
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::debug!("accept serveur HLS local: {e}");
                        continue;
                    }
                };
                let source = source.clone();
                let tok = tok.clone();
                conns.spawn(async move {
                    let svc = service_fn(move |req: Request<Incoming>| {
                        let source = source.clone();
                        let tok = tok.clone();
                        async move { Ok::<_, Infallible>(handle(req, &tok, source).await) }
                    });
                    if let Err(e) = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), svc)
                        .await
                    {
                        tracing::debug!("connexion serveur HLS local: {e}");
                    }
                });
                // Évacue les connexions terminées sans bloquer.
                while conns.try_join_next().is_some() {}
            }
        });
        Ok(Self {
            port,
            token,
            accept_task,
        })
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/{}/index.m3u8", self.port, self.token)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Arrête le serveur : listener fermé (port libéré), connexions annulées.
    pub fn shutdown(self) {
        drop(self);
    }
}

impl Drop for LocalHttpServer {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

fn response(status: StatusCode) -> Response<Full<Bytes>> {
    let mut r = Response::new(Full::new(Bytes::new()));
    *r.status_mut() = status;
    r
}

async fn handle(
    req: Request<Incoming>,
    token: &str,
    source: Arc<dyn SegmentSource>,
) -> Response<Full<Bytes>> {
    let head_only = match *req.method() {
        Method::GET => false,
        Method::HEAD => true,
        _ => return response(StatusCode::METHOD_NOT_ALLOWED),
    };
    let path = req.uri().path().to_string();
    let Some(rest) = path
        .strip_prefix('/')
        .and_then(|p| p.strip_prefix(token))
        .and_then(|p| p.strip_prefix('/'))
    else {
        return response(StatusCode::NOT_FOUND);
    };

    if rest == "index.m3u8" {
        let body = source.playlist().into_bytes();
        return full(head_only, "application/vnd.apple.mpegurl", body, None);
    }
    let Some(index) = rest
        .strip_suffix(".ts")
        .and_then(|n| n.parse::<usize>().ok())
    else {
        return response(StatusCode::NOT_FOUND);
    };

    let bytes = match tokio::time::timeout(STREAM_REQUEST_TIMEOUT, source.segment(index)).await {
        Ok(Ok(b)) => b,
        Ok(Err(SegmentError::Forbidden)) => return response(StatusCode::FORBIDDEN),
        Ok(Err(SegmentError::NotFound)) => return response(StatusCode::NOT_FOUND),
        Ok(Err(SegmentError::Timeout)) | Err(_) => {
            return response(StatusCode::SERVICE_UNAVAILABLE)
        }
        Ok(Err(SegmentError::Internal(e))) => {
            tracing::warn!("segment {index}: {e}");
            return response(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    let range = req
        .headers()
        .get(RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|v| parse_range(v, bytes.len()));
    match range {
        None => full(head_only, "video/mp2t", bytes, None),
        Some(Some((start, end))) => {
            let total = bytes.len();
            let slice = bytes[start..=end].to_vec();
            full(head_only, "video/mp2t", slice, Some((start, end, total)))
        }
        Some(None) => {
            let mut r = response(StatusCode::RANGE_NOT_SATISFIABLE);
            r.headers_mut().insert(
                CONTENT_RANGE,
                format!("bytes */{}", bytes.len())
                    .parse()
                    .expect("en-tête ascii"),
            );
            r
        }
    }
}

/// `bytes=a-b` / `bytes=a-` / `bytes=-n` (un seul intervalle). `None` =
/// insatisfaisable.
fn parse_range(value: &str, len: usize) -> Option<(usize, usize)> {
    let spec = value.strip_prefix("bytes=")?;
    if spec.contains(',') || len == 0 {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (start, end) = match (a.trim(), b.trim()) {
        ("", n) => {
            let n: usize = n.parse().ok()?;
            if n == 0 {
                return None;
            }
            (len.saturating_sub(n), len - 1)
        }
        (a, "") => (a.parse().ok()?, len - 1),
        (a, b) => (a.parse().ok()?, b.parse::<usize>().ok()?.min(len - 1)),
    };
    (start <= end && start < len).then_some((start, end))
}

fn full(
    head_only: bool,
    content_type: &str,
    body: Vec<u8>,
    range: Option<(usize, usize, usize)>,
) -> Response<Full<Bytes>> {
    let len = body.len();
    let mut r = Response::new(Full::new(if head_only {
        Bytes::new()
    } else {
        Bytes::from(body)
    }));
    if range.is_some() {
        *r.status_mut() = StatusCode::PARTIAL_CONTENT;
    }
    let h = r.headers_mut();
    h.insert(CONTENT_TYPE, content_type.parse().expect("en-tête ascii"));
    h.insert(
        CONTENT_LENGTH,
        len.to_string().parse().expect("en-tête ascii"),
    );
    h.insert(ACCEPT_RANGES, "bytes".parse().expect("en-tête ascii"));
    h.insert(CACHE_CONTROL, "no-store".parse().expect("en-tête ascii"));
    if let Some((start, end, total)) = range {
        h.insert(
            CONTENT_RANGE,
            format!("bytes {start}-{end}/{total}")
                .parse()
                .expect("en-tête ascii"),
        );
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct Fake;
    impl SegmentSource for Fake {
        fn playlist(&self) -> String {
            "#EXTM3U\n0.ts\n".to_string()
        }
        fn segment(&self, index: usize) -> BoxFuture<'static, Result<Vec<u8>, SegmentError>> {
            Box::pin(async move {
                match index {
                    0 => Ok(b"0123456789".to_vec()),
                    1 => {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        Ok(b"late".to_vec())
                    }
                    2 => Err(SegmentError::Forbidden),
                    3 => Err(SegmentError::Timeout),
                    _ => Err(SegmentError::NotFound),
                }
            })
        }
    }

    async fn server() -> LocalHttpServer {
        LocalHttpServer::start(Arc::new(Fake)).await.unwrap()
    }

    fn base(s: &LocalHttpServer) -> String {
        s.url().trim_end_matches("index.m3u8").to_string()
    }

    #[tokio::test]
    async fn serves_playlist_and_segment() {
        let s = server().await;
        let c = reqwest::Client::new();
        let r = c.get(s.url()).send().await.unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()["content-type"], "application/vnd.apple.mpegurl");
        assert_eq!(r.text().await.unwrap(), "#EXTM3U\n0.ts\n");
        let r = c.get(format!("{}0.ts", base(&s))).send().await.unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()["content-type"], "video/mp2t");
        assert_eq!(r.headers()["content-length"], "10");
        assert_eq!(r.bytes().await.unwrap().as_ref(), b"0123456789");
    }

    #[tokio::test]
    async fn wrong_token_is_404() {
        let s = server().await;
        let url = format!("http://127.0.0.1:{}/deadbeef/index.m3u8", s.port());
        assert_eq!(reqwest::get(url).await.unwrap().status(), 404);
    }

    #[tokio::test]
    async fn range_requests() {
        let s = server().await;
        let c = reqwest::Client::new();
        let r = c
            .get(format!("{}0.ts", base(&s)))
            .header("Range", "bytes=2-4")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 206);
        assert_eq!(r.headers()["content-range"], "bytes 2-4/10");
        assert_eq!(r.bytes().await.unwrap().as_ref(), b"234");
        let r = c
            .get(format!("{}0.ts", base(&s)))
            .header("Range", "bytes=7-")
            .send()
            .await
            .unwrap();
        assert_eq!(r.bytes().await.unwrap().as_ref(), b"789");
        let r = c
            .get(format!("{}0.ts", base(&s)))
            .header("Range", "bytes=50-60")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 416);
    }

    #[tokio::test]
    async fn pending_segment_is_served_when_ready_and_errors_map_to_status() {
        let s = server().await;
        let c = reqwest::Client::new();
        let r = c.get(format!("{}1.ts", base(&s))).send().await.unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.bytes().await.unwrap().as_ref(), b"late");
        assert_eq!(
            c.get(format!("{}2.ts", base(&s)))
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        assert_eq!(
            c.get(format!("{}3.ts", base(&s)))
                .send()
                .await
                .unwrap()
                .status(),
            503
        );
        assert_eq!(
            c.get(format!("{}9.ts", base(&s)))
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
        assert_eq!(
            c.get(format!("{}x.ts", base(&s)))
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
        assert_eq!(c.post(s.url()).send().await.unwrap().status(), 405);
    }

    #[tokio::test]
    async fn shutdown_frees_the_port() {
        let s = server().await;
        let port = s.port();
        s.shutdown();
        // Le port doit être réutilisable rapidement : l'abort de la tâche
        // d'accept est asynchrone, donc on retente pendant jusqu'à 2 s.
        let mut result = tokio::net::TcpListener::bind(("127.0.0.1", port)).await;
        for _ in 0..40 {
            if result.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            result = tokio::net::TcpListener::bind(("127.0.0.1", port)).await;
        }
        assert!(result.is_ok());
    }
}
