use crate::types::UnifiedTelemetry;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Thread-safe ring buffer storing recent telemetry snapshots.
#[derive(Clone)]
pub struct TelemetryCache {
    current: Arc<RwLock<UnifiedTelemetry>>,
    history: Arc<RwLock<VecDeque<UnifiedTelemetry>>>,
    capacity: usize,
}

impl TelemetryCache {
    /// Create a new TelemetryCache with the designated sample capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            current: Arc::new(RwLock::new(UnifiedTelemetry::default())),
            history: Arc::new(RwLock::new(VecDeque::with_capacity(capacity))),
            capacity: capacity.max(1),
        }
    }

    /// Push a newly scraped telemetry sample into current and append to history.
    pub async fn update(&self, sample: UnifiedTelemetry) {
        {
            let mut cur = self.current.write().await;
            *cur = sample.clone();
        }
        let mut hist = self.history.write().await;
        if hist.len() >= self.capacity {
            hist.pop_front();
        }
        hist.push_back(sample);
    }

    /// Retrieve the most recently recorded telemetry sample.
    pub async fn snapshot(&self) -> UnifiedTelemetry {
        self.current.read().await.clone()
    }

    /// Retrieve historical samples, optionally filtered to the last `seconds`.
    pub async fn history(&self, seconds: Option<u64>) -> Vec<UnifiedTelemetry> {
        let hist = self.history.read().await;
        if let Some(secs) = seconds {
            let now = chrono::Utc::now();
            hist.iter()
                .filter(|s| {
                    let diff = (now - s.timestamp).num_seconds();
                    diff >= 0 && (diff as u64) <= secs
                })
                .cloned()
                .collect()
        } else {
            hist.iter().cloned().collect()
        }
    }
}
