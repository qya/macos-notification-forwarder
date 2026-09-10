//! Deduplication (PRD §15, AT-03, AT-04).
//!
//! Key insight (PRD §12): Notification Center may **reuse** an AX element and
//! update its contents, so identity must be content-based:
//!
//! ```text
//! Same AX ID + new title/message = new notification
//! ```
//!
//! Hence the fingerprint covers content only:
//! `SHA256(app_name + "\0" + title + "\0" + message)`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

/// Compute the content fingerprint for a notification.
pub fn fingerprint(app_name: &str, title: &str, message: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(app_name.as_bytes());
    hasher.update([0]);
    hasher.update(title.as_bytes());
    hasher.update([0]);
    hasher.update(message.as_bytes());
    hex_encode(hasher.finalize())
}

fn hex_encode(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Bounded TTL cache of recently-seen fingerprints.
pub struct DedupCache {
    ttl: Duration,
    capacity: usize,
    seen: HashMap<String, Instant>,
}

impl DedupCache {
    pub fn new(ttl: Duration, capacity: usize) -> Self {
        Self {
            ttl,
            capacity: capacity.max(16),
            seen: HashMap::new(),
        }
    }

    /// Returns `true` if this fingerprint was seen recently (duplicate → ignore).
    /// Otherwise records it and returns `false` (new → forward).
    ///
    /// Content change ⇒ different fingerprint ⇒ not a duplicate (AT-04):
    /// `AAA → send, AAA → ignore, BBB → send`.
    pub fn is_duplicate(&mut self, fingerprint: &str) -> bool {
        self.evict_expired();
        if let Some(seen_at) = self.seen.get(fingerprint) {
            if seen_at.elapsed() < self.ttl {
                return true;
            }
        }
        if self.seen.len() >= self.capacity {
            // Bounded cache: evict the oldest entry (clock-scan is O(n) but
            // capacity is small and hits are infrequent).
            if let Some(oldest) = self
                .seen
                .iter()
                .min_by_key(|(_, t)| *t)
                .map(|(k, _)| k.clone())
            {
                self.seen.remove(&oldest);
            }
        }
        self.seen.insert(fingerprint.to_string(), Instant::now());
        false
    }

    fn evict_expired(&mut self) {
        let ttl = self.ttl;
        self.seen.retain(|_, t| t.elapsed() < ttl);
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_sequence_from_prd() {
        let mut cache = DedupCache::new(Duration::from_secs(900), 100);
        // ID: ABC — AAA → send, AAA → ignore, BBB → send, CCC → send.
        assert!(!cache.is_duplicate(&fingerprint("WhatsApp", "Niles", "AAA")));
        assert!(cache.is_duplicate(&fingerprint("WhatsApp", "Niles", "AAA")));
        assert!(!cache.is_duplicate(&fingerprint("WhatsApp", "Niles", "BBB")));
        assert!(!cache.is_duplicate(&fingerprint("WhatsApp", "Niles", "CCC")));
    }

    #[test]
    fn ttl_expiry_allows_resend() {
        let mut cache = DedupCache::new(Duration::from_millis(10), 100);
        let fp = fingerprint("A", "T", "M");
        assert!(!cache.is_duplicate(&fp));
        std::thread::sleep(Duration::from_millis(20));
        assert!(!cache.is_duplicate(&fp));
    }

    #[test]
    fn cache_is_bounded() {
        let mut cache = DedupCache::new(Duration::from_secs(900), 32);
        for i in 0..100 {
            cache.is_duplicate(&fingerprint("A", "T", &i.to_string()));
        }
        assert!(cache.len() <= 32, "len={}", cache.len());
    }
}
