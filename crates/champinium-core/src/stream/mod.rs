//! Lecture progressive : session HLS servie par un serveur HTTP local
//! (spec 2026-09-05). `scheduler` = logique pure, `server` = HTTP, ici =
//! l'état de session et la boucle de récupération.

pub mod scheduler;
pub mod server;

use crate::blockstore::Blockstore;
use crate::error::CoreError;
use crate::ingest::HlsManifest;
use crate::p2p::{Fetcher, StorePolicy};
use crate::seeding::SeededPublication;
use cid::Cid;
use futures::future::BoxFuture;
use libp2p::PeerId;
use scheduler::{FailureCause, SegmentScheduler, SegmentState, RETRY_DELAY};
use server::{LocalHttpServer, SegmentError, SegmentSource};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::{broadcast, mpsc, watch, Notify};
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
    /// Root de la publication lue : les segments n'étant plus annoncés dans
    /// la DHT (annonce par racine, ADR 0012), c'est l'indice passé à
    /// `Fetcher::get_with` pour les découvrir chez les fournisseurs du
    /// manifeste.
    manifest_cid: Cid,
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
    /// Émetteur à indexer à la complétion pour une session `Seed` **hors
    /// abonnement** (« seed de ce que je regarde », spec persistance §3).
    /// `None` pour un channel souscrit : le round-robin du seed proactif le
    /// visite déjà, un simple `seed_wake` suffit. `None` aussi quand aucun
    /// émetteur n'est identifiable (manifeste hors catalogue).
    issuer_to_index: Option<PeerId>,
    /// Demande d'indexation immédiate d'une publication précise, adressée à
    /// `seed_loop` — voir `Node::seed_now`.
    seed_now: mpsc::Sender<(PeerId, Cid)>,
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
                let (current, frozen) = {
                    let s = state.lock();
                    (s.state(index).clone(), s.failed_reason().is_some())
                };
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
                    // Déjà en cours de récupération (tâche lancée avant que
                    // la session ne gèle) : elle peut encore aboutir, on
                    // continue d'attendre son issue normalement.
                    SegmentState::InFlight => {}
                    // `Absent`, ou `Failed` non modéré (`NoProviders`/
                    // `Other`) : si la session est figée, `next_to_fetch` ne
                    // planifiera plus jamais sa récupération — il n'arrivera
                    // donc jamais. Sans ce test, une requête sur n'importe
                    // quel segment absent attendrait le
                    // `STREAM_REQUEST_TIMEOUT` complet avant un 503, en
                    // boucle, au lieu d'un 403 immédiat.
                    _ if frozen => return Err(SegmentError::Forbidden),
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
        manifest_cid: Cid,
        manifest: &HlsManifest,
        policy: StorePolicy,
        dir: PathBuf,
        blockstore: Blockstore,
        fetcher: Fetcher,
        events: broadcast::Sender<u64>,
        seed_wake: broadcast::Sender<()>,
        issuer_to_index: Option<PeerId>,
        seed_now: mpsc::Sender<(PeerId, Cid)>,
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
            manifest_cid,
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
            issuer_to_index,
            seed_now,
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

    /// Publication dont les blocs sont à purger si cette session se ferme
    /// maintenant (spec persistance §4) — `Some` seulement pour une session
    /// `Seed` **hors abonnement** (`issuer_to_index`) qui n'est **pas
    /// complète** : elle n'a donc rien demandé à `seed_loop` et n'entrera
    /// jamais à l'index toute seule. Complète, elle a émis sa demande
    /// d'indexation : c'est `seed_loop` qui décide (et purge lui-même si le
    /// quota la refuse). Souscrite, rien à purger : le seed proactif
    /// complètera la publication plus tard.
    ///
    /// `total_bytes`/`order` sont nuls : la valeur ne sert qu'à énumérer des
    /// CIDs pour `remove_unshared_blocks`, jamais à entrer dans un index.
    pub(crate) fn unindexed_watched_publication(&self) -> Option<SeededPublication> {
        let issuer_watched = self.state.issuer_to_index.is_some();
        if self.state.policy != StorePolicy::Seed || !issuer_watched {
            return None;
        }
        let complete = {
            let s = self.state.lock();
            s.fetched() == s.total()
        };
        if complete {
            return None;
        }
        Some(SeededPublication {
            manifest_cid: self.state.manifest_cid.to_string(),
            segment_cids: self.state.segments.iter().map(Cid::to_string).collect(),
            total_bytes: 0,
            order: 0,
        })
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
            // Indice de racine (ADR 0012) : cloné AVANT le spawn — la tâche
            // ne doit rien emprunter à `state`.
            let root = state.manifest_cid;
            let handle =
                inflight.spawn(async move { (idx, f.get_with(cid, policy, Some(root)).await) });
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
                    match state.issuer_to_index {
                        // Hors abonnement (« ce que je regarde ») : le
                        // round-robin du seed proactif ne visiterait jamais
                        // cet émetteur — on nomme la publication à indexer.
                        Some(issuer) => {
                            if state.seed_now.try_send((issuer, state.manifest_cid)).is_err() {
                                tracing::debug!(
                                    "session {}: demande d'indexation non transmise (canal saturé ou fermé)",
                                    state.id
                                );
                            }
                        }
                        // Channel souscrit : le seed proactif s'en charge.
                        None => {
                            let _ = state.seed_wake.send(());
                        }
                    }
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
