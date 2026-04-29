// SPDX-License-Identifier: MIT
//! Bridges the emulator-thread `ResourceEvent` broadcast onto MCP
//! `notifications/resources/updated` for every connected client peer.
//!
//! Per MCP 2024-11-05, the notification body carries only `{ uri }` —
//! clients fetch fresh content via `resources/read`. To keep `read` calls
//! cheap (and off the emulator thread on the hot path), we cache the most
//! recent broadcast payload per URI in a `parking_lot::RwLock`. Resource
//! reads consult that cache first and fall back to the actor.
//!
//! A per-URI rate limiter throttles emission to at most `min_interval_ms`
//! apart, configurable via `--ui-refresh-hz`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use parking_lot::{Mutex, RwLock};
use rmcp::model::ResourceUpdatedNotificationParam;
use rmcp::service::Peer;
use rmcp::RoleServer;
use tokio::sync::broadcast;

use crate::emulator::{EmulatorActor, ResourceEvent};

/// Per-URI cached payload (the most recent broadcast snapshot).
pub type SnapshotCache = Arc<RwLock<HashMap<&'static str, (Bytes, &'static str)>>>;

#[derive(Clone)]
pub struct Notifier {
    cache: SnapshotCache,
    peers: Arc<Mutex<Vec<Peer<RoleServer>>>>,
    min_interval: Duration,
}

impl Notifier {
    pub fn new(min_interval: Duration) -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
            peers: Arc::new(Mutex::new(Vec::new())),
            min_interval,
        }
    }

    pub fn cache(&self) -> SnapshotCache {
        self.cache.clone()
    }

    pub fn register_peer(&self, peer: Peer<RoleServer>) {
        let mut guard = self.peers.lock();
        // Prune dead peers opportunistically.
        guard.retain(|p| !p.is_transport_closed());
        guard.push(peer);
    }

    /// Spawn the broadcast → peer-fanout pump. Returns immediately; the
    /// background task lives until the broadcast channel closes.
    pub fn spawn(&self, actor: &EmulatorActor) {
        let mut rx = actor.subscribe();
        let cache = self.cache.clone();
        let peers = self.peers.clone();
        let min_interval = self.min_interval;
        tokio::spawn(async move {
            let mut last_emit: HashMap<&'static str, Instant> = HashMap::new();
            loop {
                let evt = match rx.recv().await {
                    Ok(e) => e,
                    Err(broadcast::error::RecvError::Closed) => return,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                };
                let ResourceEvent { uri, mime, payload } = evt;
                let now = Instant::now();
                if let Some(prev) = last_emit.get(uri) {
                    if now.duration_since(*prev) < min_interval {
                        // Still update the cache so a follow-up read sees fresh data,
                        // but skip the notification.
                        cache
                            .write()
                            .insert(uri, (Bytes::from(payload.as_ref().clone()), mime));
                        continue;
                    }
                }
                last_emit.insert(uri, now);
                cache
                    .write()
                    .insert(uri, (Bytes::from(payload.as_ref().clone()), mime));

                let snapshot: Vec<Peer<RoleServer>> = {
                    let mut g = peers.lock();
                    g.retain(|p| !p.is_transport_closed());
                    g.clone()
                };
                for peer in snapshot {
                    let params = ResourceUpdatedNotificationParam {
                        uri: uri.to_string(),
                    };
                    if let Err(e) = peer.notify_resource_updated(params).await {
                        tracing::debug!(uri, error = %e, "notify_resource_updated failed");
                    }
                }
            }
        });
    }
}

/// Minimum interval (ms) for a refresh rate in Hz, clamped to 1..=30.
pub fn min_interval_for_hz(hz: u32) -> Duration {
    let clamped = hz.clamp(1, 30);
    Duration::from_millis(1000 / clamped as u64)
}
