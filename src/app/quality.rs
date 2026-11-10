//! Per-node quality history used to rank failover candidates.
//!
//! The score is an additive penalty on top of the most recent measured delay:
//! jitter and recent failures make a node less attractive even when its best
//! probe looks fast. The regular urltest selection stays in sing-box; this
//! history only decides which candidates DFRelay tests first and which node a
//! manual-mode recovery should pin.

use std::collections::{HashMap, HashSet};

const DELAY_ALPHA: f64 = 0.3;
const JITTER_ALPHA: f64 = 0.25;
const RELIABILITY_RECOVERY: f64 = 0.2;
const RELIABILITY_DECAY: f64 = 0.75;
const JITTER_WEIGHT: f64 = 1.5;
const FAILURE_PENALTY_MS: f64 = 600.0;
const FAILURE_STREAK_PENALTY_MS: f64 = 100.0;
const UNKNOWN_SCORE_MS: f64 = 1000.0;

#[derive(Debug, Clone)]
pub(super) struct NodeQuality {
    ewma_delay: f64,
    jitter: f64,
    reliability: f64,
    consecutive_failures: u32,
    samples: u64,
}

impl NodeQuality {
    fn new() -> Self {
        Self {
            ewma_delay: 0.0,
            jitter: 0.0,
            reliability: 1.0,
            consecutive_failures: 0,
            samples: 0,
        }
    }

    fn record_success(&mut self, delay: u64) {
        let delay = delay as f64;
        if self.samples == 0 {
            self.ewma_delay = delay;
        } else {
            self.jitter =
                JITTER_ALPHA * (delay - self.ewma_delay).abs() + (1.0 - JITTER_ALPHA) * self.jitter;
            self.ewma_delay = DELAY_ALPHA * delay + (1.0 - DELAY_ALPHA) * self.ewma_delay;
        }
        self.reliability += (1.0 - self.reliability) * RELIABILITY_RECOVERY;
        self.consecutive_failures = 0;
        self.samples = self.samples.saturating_add(1);
    }

    fn record_failure(&mut self) {
        self.reliability *= RELIABILITY_DECAY;
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.samples = self.samples.saturating_add(1);
    }

    fn score(&self, measured_delay: Option<u64>) -> f64 {
        let base = measured_delay
            .map(|delay| delay as f64)
            .or_else(|| (self.samples > 0).then_some(self.ewma_delay))
            .unwrap_or(UNKNOWN_SCORE_MS);
        base + self.jitter * JITTER_WEIGHT
            + (1.0 - self.reliability) * FAILURE_PENALTY_MS
            + f64::from(self.consecutive_failures.min(5)) * FAILURE_STREAK_PENALTY_MS
    }
}

#[derive(Debug, Default)]
pub(super) struct QualityBook {
    entries: HashMap<String, NodeQuality>,
}

impl QualityBook {
    pub(super) fn record_success(&mut self, tag: &str, delay: u64) {
        self.entries
            .entry(tag.to_string())
            .or_insert_with(NodeQuality::new)
            .record_success(delay);
    }

    pub(super) fn record_failure(&mut self, tag: &str) {
        self.entries
            .entry(tag.to_string())
            .or_insert_with(NodeQuality::new)
            .record_failure();
    }

    /// Lower is better. `measured_delay` is the delay from the probe that just
    /// succeeded; historical reliability and jitter are applied on top.
    pub(super) fn score(&self, tag: &str, measured_delay: Option<u64>) -> f64 {
        match self.entries.get(tag) {
            Some(quality) => quality.score(measured_delay),
            None => measured_delay
                .map(|delay| delay as f64)
                .unwrap_or(UNKNOWN_SCORE_MS),
        }
    }

    pub(super) fn prune(&mut self, tags: &HashSet<String>) {
        self.entries.retain(|tag, _| tags.contains(tag));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_make_a_fast_node_less_attractive() {
        let mut book = QualityBook::default();
        for _ in 0..3 {
            book.record_success("n-steady", 80);
            book.record_success("n-flaky", 80);
        }
        for _ in 0..3 {
            book.record_failure("n-flaky");
        }
        assert!(
            book.score("n-flaky", Some(80)) > book.score("n-steady", Some(80)),
            "failures must add a penalty"
        );
    }

    #[test]
    fn jitter_raises_the_score_at_equal_recent_delay() {
        let mut book = QualityBook::default();
        for delay in [100, 100, 100, 100] {
            book.record_success("n-steady", delay);
        }
        for delay in [40, 160, 40, 160] {
            book.record_success("n-jumpy", delay);
        }
        assert!(
            book.score("n-jumpy", Some(100)) > book.score("n-steady", Some(100)),
            "jitter must add a penalty"
        );
    }

    #[test]
    fn successes_restore_reliability() {
        let mut book = QualityBook::default();
        for _ in 0..4 {
            book.record_failure("n1");
        }
        let degraded = book.score("n1", Some(100));
        for _ in 0..6 {
            book.record_success("n1", 100);
        }
        assert!(book.score("n1", Some(100)) < degraded);
    }

    #[test]
    fn unknown_nodes_fall_back_to_the_measured_delay() {
        let book = QualityBook::default();
        assert_eq!(book.score("n1", Some(42)), 42.0);
        assert_eq!(book.score("n1", None), UNKNOWN_SCORE_MS);
    }

    #[test]
    fn prune_drops_removed_nodes() {
        let mut book = QualityBook::default();
        book.record_success("keep", 40);
        book.record_success("drop", 40);
        book.prune(&HashSet::from(["keep".to_string()]));
        assert_eq!(book.score("drop", None), UNKNOWN_SCORE_MS);
        assert_eq!(book.score("keep", None), 40.0);
    }
}
