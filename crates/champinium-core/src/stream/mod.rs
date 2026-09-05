//! Lecture progressive : session HLS servie par un serveur HTTP local
//! (spec 2026-09-05). `scheduler` = logique pure, `server` = HTTP, ici =
//! l'état de session et la boucle de récupération.

pub mod scheduler;
pub mod server;

use crate::blockstore::Blockstore;
use crate::error::CoreError;
use crate::ingest::HlsManifest;
use crate::p2p::{Fetcher, StorePolicy};
use cid::Cid;
use futures::future::BoxFuture;
use scheduler::{FailureCause, SegmentScheduler, SegmentState, RETRY_DELAY};
use server::{LocalHttpServer, SegmentError, SegmentSource};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::{broadcast, watch, Notify};
use tokio::task::{JoinHandle, JoinSet};

/// Résultat d'`open_stream` : l'URL à donner telle quelle au lecteur natif.
#[derive(Debug, Clone)]
pub struct StreamSessionInfo {
    pub id: u64,
    pub url: String,
    pub total_segments: u32,
}

/// État observable d'une session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamStatus {
    pub fetched_segments: u32,
    pub total_segments: u32,
    pub failed_reason: Option<String>,
}

/// Répertoire parent des sessions : `<blocs>/.streams/`, purgé à l'ouverture
/// du nœud (sessions orphelines d'un front qui n'a pas fermé).
pub fn streams_root(blockstore: &Blockstore) -> PathBuf {
    blockstore.root().join(".streams")
}

/// État partagé entre le serveur (lecture) et la boucle de fetch (écriture).
pub(crate) struct SessionState {
    id: u64,
    segments: Vec<Cid>,
    playlist: String,
    policy: StorePolicy,
    dir: PathBuf,
    blockstore: Blockstore,
    scheduler: Mutex<SegmentScheduler>,
    /// Réveil de la boucle de fetch (nouvelle demande du lecteur).
    wake: Notify,
    /// Compteur de changements d'état (les requêtes en attente l'observent).
    changed: watch::Sender<u64>,
    /// Tic vers `Node::subscribe_stream` (listener FFI).
    events: broadcast::Sender<u64>,
    /// Réveil du seed proactif à la complétion d'une session `Seed` : la
    /// boucle de seed enregistre alors la publication au `SeedIndex` sous
    /// quota (blocs déjà en cache → passe peu coûteuse).
    seed_wake: broadcast::Sender<()>,
    /// Poignée faible sur soi-même : `SegmentSource::segment` prend `&self`
    /// mais doit rendre un futur `'static`. Posée juste après la construction
    /// de l'`Arc` (voir [`StreamSession::open`]).
    me: Mutex<std::sync::Weak<SessionState>>,
}

impl SessionState {
    fn lock(&self) -> std::sync::MutexGuard<'_, SegmentScheduler> {
        self.scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// L'`expect` est sûr : le seul appelant est le serveur HTTP, arrêté (et
    /// ses connexions annulées) avant que la session — qui détient l'`Arc` —
    /// soit lâchée.
    fn self_arc(&self) -> Arc<SessionState> {
        self.me
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .upgrade()
            .expect("SessionState toujours détenu par sa session tant que le serveur tourne")
    }

    fn notify_changed(&self) {
        self.changed.send_modify(|c| *c += 1);
        let _ = self.events.send(self.id);
    }

    pub(crate) fn status(&self) -> StreamStatus {
        let s = self.lock();
        StreamStatus {
            fetched_segments: s.fetched() as u32,
            total_segments: s.total() as u32,
            failed_reason: s.failed_reason(),
        }
    }

    /// Lecture **bloquante** des octets locaux : `Blockstore::get` fait un
    /// `std::fs::read` complet suivi d'une re-vérification SHA-256 du CID, et
    /// un segment vidéo pèse des mégaoctets. À n'appeler que depuis un
    /// contexte bloquant — voir [`SessionState::read_local`].
    fn local_bytes(&self, index: usize) -> Option<Vec<u8>> {
        let cid = self.segments.get(index)?;
        if self.blockstore.has(cid) {
            return self.blockstore.get(cid).ok();
        }
        std::fs::read(self.dir.join(format!("{index}.ts"))).ok()
    }

    /// [`SessionState::local_bytes`] déportée sur le pool bloquant : appelée
    /// depuis la tâche de connexion HTTP, qui partage son runtime avec la
    /// boucle d'évènements libp2p — une lecture multi-mégaoctets + SHA-256 y
    /// bloquerait le swarm. Le verrou de l'ordonnanceur n'est jamais tenu
    /// pendant l'attente (aucun appelant ne le détient à ce point).
    async fn read_local(
        state: Arc<SessionState>,
        index: usize,
    ) -> Result<Option<Vec<u8>>, SegmentError> {
        tokio::task::spawn_blocking(move || state.local_bytes(index))
            .await
            .map_err(|e| SegmentError::Internal(format!("lecture locale interrompue: {e}")))
    }
}

impl SegmentSource for SessionState {
    fn playlist(&self) -> String {
        self.playlist.clone()
    }

    fn segment(&self, index: usize) -> BoxFuture<'static, Result<Vec<u8>, SegmentError>> {
        if index >= self.segments.len() {
            return Box::pin(async { Err(SegmentError::NotFound) });
        }
        // La demande déplace la tête de lecture dans tous les cas. `subscribe`
        // AVANT `request` : sinon un segment devenu présent entre les deux ne
        // réveillerait jamais l'attente ci-dessous.
        let mut rx = self.changed.subscribe();
        {
            self.lock().request(index);
        }
        let state: Arc<SessionState> = self.self_arc();
        Box::pin(async move {
            // Chemin rapide : octets déjà locaux. La lecture part sur le pool
            // bloquant, jamais sur le runtime partagé avec le swarm.
            if let Some(bytes) = SessionState::read_local(state.clone(), index).await? {
                return Ok(bytes);
            }
            // Absent : la boucle de fetch doit le prendre en priorité.
            state.wake.notify_one();
            loop {
                // L'état est cloné dans un `let` avant le `match` : le verrou
                // tombe ainsi dès la fin de l'instruction, jamais pendant la
                // lecture des octets.
                let current = state.lock().state(index).clone();
                match current {
                    SegmentState::Present => {
                        return SessionState::read_local(state.clone(), index).await?.ok_or(
                            SegmentError::Internal("segment marqué présent mais illisible".into()),
                        );
                    }
                    SegmentState::Failed {
                        cause: FailureCause::Moderated,
                        ..
                    } => return Err(SegmentError::Forbidden),
                    _ => {}
                }
                if rx.changed().await.is_err() {
                    return Err(SegmentError::Internal("session fermée".into()));
                }
            }
        })
    }
}

/// Une session ouverte. Lâcher la valeur arrête serveur et boucle de fetch.
pub(crate) struct StreamSession {
    /// Jamais relu : sa seule raison d'être est de garder le serveur en vie
    /// aussi longtemps que la session (son `Drop` ferme le listener et annule
    /// les connexions en cours).
    #[allow(dead_code)]
    server: LocalHttpServer,
    state: Arc<SessionState>,
    fetch_task: JoinHandle<()>,
}

impl Drop for StreamSession {
    fn drop(&mut self) {
        self.fetch_task.abort();
    }
}

impl StreamSession {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn open(
        id: u64,
        manifest: &HlsManifest,
        policy: StorePolicy,
        dir: PathBuf,
        blockstore: Blockstore,
        fetcher: Fetcher,
        events: broadcast::Sender<u64>,
        seed_wake: broadcast::Sender<()>,
    ) -> Result<(Self, StreamSessionInfo), CoreError> {
        let segments = manifest
            .segments
            .iter()
            .map(|s| s.cid.parse::<Cid>().map_err(CoreError::Cid))
            .collect::<Result<Vec<_>, _>>()?;
        let durations = manifest.segments.iter().map(|s| s.duration).collect();
        tokio::fs::create_dir_all(&dir).await?;
        let (changed, _) = watch::channel(0u64);
        let state = Arc::new(SessionState {
            id,
            segments,
            playlist: manifest.to_m3u8_with_uris(|i, _| format!("{i}.ts")),
            policy,
            dir,
            blockstore,
            scheduler: Mutex::new(SegmentScheduler::new(durations)),
            wake: Notify::new(),
            changed,
            events,
            seed_wake,
            me: Mutex::new(std::sync::Weak::new()),
        });
        *state
            .me
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::downgrade(&state);
        let server = LocalHttpServer::start(state.clone()).await?;
        let fetch_task = tokio::spawn(fetch_loop(fetcher, state.clone()));
        let info = StreamSessionInfo {
            id,
            url: server.url(),
            total_segments: state.segments.len() as u32,
        };
        Ok((
            Self {
                server,
                state,
                fetch_task,
            },
            info,
        ))
    }

    pub(crate) fn status(&self) -> StreamStatus {
        self.state.status()
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.state.dir
    }
}

/// Boucle de récupération d'une session. Ne détient **jamais** de `Node` (un
/// [`Fetcher`] et l'état partagé suffisent) : lâcher la dernière poignée
/// `Node` lâche la table des sessions, donc annule cette tâche.
async fn fetch_loop(fetcher: Fetcher, state: Arc<SessionState>) {
    let mut inflight: JoinSet<(usize, Result<Vec<u8>, CoreError>)> = JoinSet::new();
    // Index du segment par identifiant de tâche : sans lui, une tâche qui
    // panique rendrait un `JoinError` sans index et laisserait le segment
    // `InFlight` à jamais — jamais réessayé (l'ordonnanceur juge `InFlight`
    // inéligible) et occupant durablement un des `MAX_INFLIGHT` créneaux.
    let mut task_index: HashMap<tokio::task::Id, usize> = HashMap::new();
    let mut completed_notified = false;
    loop {
        // Réclame tout ce que l'ordonnanceur autorise.
        loop {
            let next = state.lock().next_to_fetch(Instant::now());
            let Some(idx) = next else { break };
            let f = fetcher.clone();
            let cid = state.segments[idx];
            let policy = state.policy;
            let handle = inflight.spawn(async move { (idx, f.get_with(cid, policy).await) });
            task_index.insert(handle.id(), idx);
        }

        tokio::select! {
            Some(joined) = inflight.join_next_with_id(), if !inflight.is_empty() => {
                let (idx, res) = match joined {
                    Ok((task_id, payload)) => {
                        task_index.remove(&task_id);
                        payload
                    }
                    Err(join_error) => {
                        // Tâche paniquée ou annulée : on retrouve son segment
                        // par identifiant pour le marquer en échec, donc
                        // rejouable après `RETRY_DELAY`.
                        let Some(idx) = task_index.remove(&join_error.id()) else { continue };
                        tracing::warn!("session {}: tâche du segment {idx} interrompue: {join_error}", state.id);
                        state.lock().mark_failed(
                            idx,
                            FailureCause::Other(format!("tâche de récupération interrompue: {join_error}")),
                            Instant::now(),
                        );
                        state.notify_changed();
                        continue;
                    }
                };
                match res {
                    Ok(bytes) => {
                        if state.policy == StorePolicy::Stream {
                            if let Err(e) = tokio::fs::write(state.dir.join(format!("{idx}.ts")), &bytes).await {
                                state.lock().mark_failed(idx, FailureCause::Other(e.to_string()), Instant::now());
                                state.notify_changed();
                                continue;
                            }
                        }
                        state.lock().mark_present(idx);
                    }
                    Err(CoreError::Moderated(m)) => {
                        tracing::warn!("session {}: segment {idx} refusé par la modération ({m})", state.id);
                        state.lock().mark_failed(idx, FailureCause::Moderated, Instant::now());
                    }
                    Err(CoreError::NoProviders(_)) | Err(CoreError::BlockNotFound(_)) => {
                        state.lock().mark_failed(idx, FailureCause::NoProviders, Instant::now());
                    }
                    Err(e) => {
                        tracing::debug!("session {}: segment {idx}: {e}", state.id);
                        state.lock().mark_failed(idx, FailureCause::Other(e.to_string()), Instant::now());
                    }
                }
                state.notify_changed();
                let done = { let s = state.lock(); s.fetched() == s.total() };
                if done && !completed_notified && state.policy == StorePolicy::Seed {
                    completed_notified = true;
                    let _ = state.seed_wake.send(());
                }
            }
            _ = state.wake.notified() => {}
            _ = tokio::time::sleep(RETRY_DELAY) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Garde de la reprise après panique d'une tâche de récupération
    /// (`fetch_loop`) : l'identifiant rendu par `JoinSet::spawn` retrouve le
    /// segment dans le `JoinError`, et le marquer `Failed` le rend à nouveau
    /// éligible après `RETRY_DELAY` — sans quoi il resterait `InFlight` à
    /// jamais et retiendrait un des `MAX_INFLIGHT` créneaux.
    #[tokio::test]
    async fn panicking_fetch_task_is_traced_back_to_its_segment_and_retried() {
        let mut inflight: JoinSet<(usize, Result<Vec<u8>, CoreError>)> = JoinSet::new();
        let mut task_index: HashMap<tokio::task::Id, usize> = HashMap::new();

        let mut sched = SegmentScheduler::new(vec![1.0; 3]);
        let t0 = Instant::now();
        let idx = sched.next_to_fetch(t0).expect("un segment à récupérer");
        let handle = inflight.spawn(async { panic!("récupération en échec") });
        task_index.insert(handle.id(), idx);

        let joined = inflight
            .join_next_with_id()
            .await
            .expect("une tâche à joindre");
        let join_error = joined.expect_err("la tâche a paniqué");
        assert_eq!(
            task_index.remove(&join_error.id()),
            Some(idx),
            "le JoinError doit retrouver son segment"
        );

        sched.mark_failed(idx, FailureCause::Other("interrompue".into()), t0);
        assert_eq!(
            sched.next_to_fetch(t0),
            Some(idx + 1),
            "pas de reprise immédiate"
        );
        sched.mark_present(idx + 1);
        assert_eq!(
            sched.next_to_fetch(t0 + RETRY_DELAY),
            Some(idx),
            "le segment doit redevenir éligible après RETRY_DELAY"
        );
        assert!(
            sched.failed_reason().is_none(),
            "une panique ne fige pas la session (seule la modération le fait)"
        );
    }
}
