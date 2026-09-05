//! Ordonnanceur de segments d'une session de lecture — logique pure, sans
//! réseau ni horloge propre (l'`Instant` est injecté), testable seule.
//!
//! Priorité : (1) segments demandés par le lecteur et absents, dans l'ordre de
//! demande ; (2) fenêtre d'avance de `LOOKAHEAD_SECS` après la tête de
//! lecture ; (3) rien. Un seek déplace la tête. `Moderated` fige la session.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub const LOOKAHEAD_SECS: f32 = 90.0;
pub const MAX_INFLIGHT: usize = 3;
pub const RETRY_DELAY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureCause {
    NoProviders,
    Moderated,
    Other(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum SegmentState {
    Absent,
    InFlight,
    Present,
    Failed { cause: FailureCause, since: Instant },
}

#[derive(Debug)]
pub struct SegmentScheduler {
    durations: Vec<f32>,
    states: Vec<SegmentState>,
    head: usize,
    wanted: VecDeque<usize>,
    frozen: Option<String>,
}

impl SegmentScheduler {
    pub fn new(durations: Vec<f32>) -> Self {
        let n = durations.len();
        Self {
            durations,
            states: vec![SegmentState::Absent; n],
            head: 0,
            wanted: VecDeque::new(),
            frozen: None,
        }
    }

    pub fn total(&self) -> usize {
        self.states.len()
    }

    pub fn fetched(&self) -> usize {
        self.states
            .iter()
            .filter(|s| **s == SegmentState::Present)
            .count()
    }

    pub fn in_flight(&self) -> usize {
        self.states
            .iter()
            .filter(|s| **s == SegmentState::InFlight)
            .count()
    }

    pub fn state(&self, idx: usize) -> &SegmentState {
        &self.states[idx]
    }

    pub fn failed_reason(&self) -> Option<String> {
        self.frozen.clone()
    }

    /// Le lecteur demande `idx` : la tête se déplace ; s'il manque, il passe
    /// en tête de priorité (dédupliqué).
    pub fn request(&mut self, idx: usize) {
        if idx >= self.states.len() {
            return;
        }
        self.head = idx;
        match self.states[idx] {
            SegmentState::Present | SegmentState::InFlight => {}
            _ => {
                if !self.wanted.contains(&idx) {
                    self.wanted.push_back(idx);
                }
            }
        }
    }

    fn eligible(&self, idx: usize, now: Instant) -> bool {
        match &self.states[idx] {
            SegmentState::Absent => true,
            SegmentState::Failed { since, .. } => now.duration_since(*since) >= RETRY_DELAY,
            SegmentState::InFlight | SegmentState::Present => false,
        }
    }

    /// Réclame le prochain segment à récupérer (marqué `InFlight`), ou `None`
    /// si rien n'est éligible, si `MAX_INFLIGHT` est atteint, ou si la
    /// session est figée.
    pub fn next_to_fetch(&mut self, now: Instant) -> Option<usize> {
        if self.frozen.is_some() || self.in_flight() >= MAX_INFLIGHT {
            return None;
        }
        // (1) demandes explicites du lecteur.
        if let Some(pos) = self.wanted.iter().position(|&i| self.eligible(i, now)) {
            let idx = self.wanted.remove(pos).expect("position valide");
            self.states[idx] = SegmentState::InFlight;
            return Some(idx);
        }
        // Purge les demandes devenues présentes.
        self.wanted
            .retain(|&i| self.states[i] != SegmentState::Present);
        // (2) fenêtre d'avance après la tête.
        let mut acc = 0.0_f32;
        for idx in self.head..self.states.len() {
            if acc > LOOKAHEAD_SECS {
                break;
            }
            if self.eligible(idx, now) {
                self.states[idx] = SegmentState::InFlight;
                return Some(idx);
            }
            acc += self.durations[idx];
        }
        None
    }

    pub fn mark_present(&mut self, idx: usize) {
        self.states[idx] = SegmentState::Present;
        self.wanted.retain(|&i| i != idx);
    }

    pub fn mark_failed(&mut self, idx: usize, cause: FailureCause, now: Instant) {
        if cause == FailureCause::Moderated {
            self.frozen = Some(format!(
                "segment {idx} refusé par la modération — contenu non lisible"
            ));
        }
        self.states[idx] = SegmentState::Failed { cause, since: now };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sched(n: usize, dur: f32) -> SegmentScheduler {
        SegmentScheduler::new(vec![dur; n])
    }

    #[test]
    fn fetches_sequentially_from_start_without_requests() {
        let mut s = sched(10, 10.0);
        let now = Instant::now();
        assert_eq!(s.next_to_fetch(now), Some(0));
        assert_eq!(s.next_to_fetch(now), Some(1));
        assert_eq!(s.next_to_fetch(now), Some(2));
        // MAX_INFLIGHT = 3 atteint.
        assert_eq!(s.next_to_fetch(now), None);
        s.mark_present(0);
        assert_eq!(s.next_to_fetch(now), Some(3));
    }

    #[test]
    fn seek_puts_requested_segment_first_then_window() {
        let mut s = sched(100, 10.0);
        let now = Instant::now();
        s.request(40);
        assert_eq!(s.next_to_fetch(now), Some(40));
        assert_eq!(s.next_to_fetch(now), Some(41));
        assert_eq!(s.next_to_fetch(now), Some(42));
    }

    #[test]
    fn window_is_bounded_by_lookahead_secs() {
        // 10 s par segment : la fenêtre couvre 9 segments après la tête
        // (90 s), pas plus.
        let mut s = sched(100, 10.0);
        let now = Instant::now();
        let mut claimed = vec![];
        while let Some(i) = s.next_to_fetch(now) {
            claimed.push(i);
            s.mark_present(i);
        }
        assert_eq!(claimed, (0..=9).collect::<Vec<_>>());
    }

    #[test]
    fn no_providers_is_retried_after_delay() {
        let mut s = sched(1, 1.0);
        let t0 = Instant::now();
        assert_eq!(s.next_to_fetch(t0), Some(0));
        s.mark_failed(0, FailureCause::NoProviders, t0);
        assert_eq!(s.next_to_fetch(t0), None);
        assert_eq!(s.next_to_fetch(t0 + RETRY_DELAY), Some(0));
        assert!(s.failed_reason().is_none());
    }

    #[test]
    fn moderated_freezes_the_session() {
        let mut s = sched(3, 1.0);
        let now = Instant::now();
        assert_eq!(s.next_to_fetch(now), Some(0));
        s.mark_failed(0, FailureCause::Moderated, now);
        assert!(s.failed_reason().unwrap().contains("modération"));
        assert_eq!(s.next_to_fetch(now + RETRY_DELAY), None);
    }

    #[test]
    fn request_of_present_segment_only_moves_head() {
        let mut s = sched(5, 1.0);
        let now = Instant::now();
        assert_eq!(s.next_to_fetch(now), Some(0));
        s.mark_present(0);
        s.request(0);
        assert_eq!(s.fetched(), 1);
        assert_eq!(s.next_to_fetch(now), Some(1));
    }
}
