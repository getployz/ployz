use std::time::Duration;
use tokio::time::Instant;

/// Grace period after an endpoint change before declaring a peer down.
pub const ENDPOINT_CONNECTION_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum time since the last handshake before a peer is considered down.
pub const PEER_DOWN_INTERVAL: Duration = Duration::from_secs(275);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerStatus {
    Up,
    Down,
    Unknown,
}

/// Pure endpoint rotation state for a single WireGuard peer.
///
/// Ported from Uncloud's peer.go: tracks multiple endpoints, rotates on failure,
/// and classifies peer health using a 3-zone time model.
#[derive(Debug, Clone)]
pub struct WireGuardPeer {
    pub endpoints: Vec<String>,
    pub active_endpoint: usize,
    pub last_endpoint_change: Instant,
    pub last_handshake: Option<Instant>,
    pub status: PeerStatus,
}

impl WireGuardPeer {
    #[must_use] 
    pub fn new(endpoints: Vec<String>, now: Instant) -> Self {
        Self {
            endpoints,
            active_endpoint: 0,
            last_endpoint_change: now,
            last_handshake: None,
            status: PeerStatus::Unknown,
        }
    }

    #[must_use] 
    pub fn active_endpoint(&self) -> Option<&str> {
        self.endpoints.get(self.active_endpoint).map(|s| s.as_str())
    }

    /// Classify peer status using a 3-zone time model:
    ///
    /// - Zone 1 (0–15s after endpoint change): Unknown, unless handshake arrived → Up
    /// - Zone 2 (15s–275s): Down if no handshake since change, Up otherwise
    /// - Zone 3 (>275s): Up if handshake within 275s, Down if stale
    pub fn calculate_status(&mut self, now: Instant) {
        let since_change = now.duration_since(self.last_endpoint_change);

        if since_change < ENDPOINT_CONNECTION_TIMEOUT {
            self.status = match self.last_handshake {
                Some(hs) if hs >= self.last_endpoint_change => PeerStatus::Up,
                _ => PeerStatus::Unknown,
            };
        } else if since_change < PEER_DOWN_INTERVAL {
            self.status = match self.last_handshake {
                Some(hs) if hs >= self.last_endpoint_change => PeerStatus::Up,
                _ => PeerStatus::Down,
            };
        } else {
            self.status = match self.last_handshake {
                Some(hs) if now.duration_since(hs) < PEER_DOWN_INTERVAL => PeerStatus::Up,
                _ => PeerStatus::Down,
            };
        }
    }

    /// Returns true if the peer is Down and has multiple endpoints to rotate through.
    #[must_use] 
    pub fn should_change_endpoint(&self) -> bool {
        self.status == PeerStatus::Down && self.endpoints.len() > 1
    }

    /// Rotate to the next endpoint in the circular list.
    pub fn rotate_endpoint(&mut self, now: Instant) {
        if self.endpoints.is_empty() {
            return;
        }
        self.active_endpoint = (self.active_endpoint + 1) % self.endpoints.len();
        self.last_endpoint_change = now;
        self.status = PeerStatus::Unknown;
    }

    /// Update the endpoint list, preserving the active endpoint if still present.
    pub fn update_endpoints(&mut self, new: Vec<String>) {
        if new.is_empty() {
            self.endpoints = new;
            self.active_endpoint = 0;
            return;
        }

        let current = self.active_endpoint().map(|s| s.to_string());
        self.endpoints = new;

        if let Some(ref current) = current
            && let Some(pos) = self.endpoints.iter().position(|e| e == current)
        {
            self.active_endpoint = pos;
            return;
        }

        self.active_endpoint = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zone1_unknown_no_handshake() {
        let now = Instant::now();
        let mut peer = WireGuardPeer::new(vec!["a:1".into()], now);
        peer.calculate_status(now + Duration::from_secs(5));
        assert_eq!(peer.status, PeerStatus::Unknown);
    }

    #[test]
    fn zone1_up_with_handshake() {
        let now = Instant::now();
        let mut peer = WireGuardPeer::new(vec!["a:1".into()], now);
        peer.last_handshake = Some(now + Duration::from_secs(2));
        peer.calculate_status(now + Duration::from_secs(5));
        assert_eq!(peer.status, PeerStatus::Up);
    }

    #[test]
    fn zone2_down_no_handshake() {
        let now = Instant::now();
        let mut peer = WireGuardPeer::new(vec!["a:1".into()], now);
        peer.calculate_status(now + Duration::from_secs(20));
        assert_eq!(peer.status, PeerStatus::Down);
    }

    #[test]
    fn zone2_up_with_handshake_after_change() {
        let now = Instant::now();
        let mut peer = WireGuardPeer::new(vec!["a:1".into()], now);
        peer.last_handshake = Some(now + Duration::from_secs(5));
        peer.calculate_status(now + Duration::from_secs(20));
        assert_eq!(peer.status, PeerStatus::Up);
    }

    #[test]
    fn zone2_down_with_handshake_before_change() {
        let now = Instant::now();
        let mut peer = WireGuardPeer::new(vec!["a:1".into()], now + Duration::from_secs(10));
        peer.last_handshake = Some(now + Duration::from_secs(5));
        peer.calculate_status(now + Duration::from_secs(30));
        assert_eq!(peer.status, PeerStatus::Down);
    }

    #[test]
    fn zone3_up_recent_handshake() {
        let now = Instant::now();
        let mut peer = WireGuardPeer::new(vec!["a:1".into()], now);
        peer.last_handshake = Some(now + Duration::from_secs(200));
        peer.calculate_status(now + Duration::from_secs(300));
        assert_eq!(peer.status, PeerStatus::Up);
    }

    #[test]
    fn zone3_down_stale_handshake() {
        let now = Instant::now();
        let mut peer = WireGuardPeer::new(vec!["a:1".into()], now);
        peer.last_handshake = Some(now + Duration::from_secs(10));
        peer.calculate_status(now + Duration::from_secs(300));
        assert_eq!(peer.status, PeerStatus::Down);
    }

    #[test]
    fn zone3_down_no_handshake() {
        let now = Instant::now();
        let mut peer = WireGuardPeer::new(vec!["a:1".into()], now);
        peer.calculate_status(now + Duration::from_secs(300));
        assert_eq!(peer.status, PeerStatus::Down);
    }

    #[test]
    fn should_change_only_when_down_multi_endpoint() {
        let now = Instant::now();
        let mut peer = WireGuardPeer::new(vec!["a:1".into(), "b:2".into()], now);

        peer.status = PeerStatus::Down;
        assert!(peer.should_change_endpoint());

        peer.status = PeerStatus::Up;
        assert!(!peer.should_change_endpoint());

        peer.status = PeerStatus::Unknown;
        assert!(!peer.should_change_endpoint());
    }

    #[test]
    fn should_not_change_single_endpoint() {
        let now = Instant::now();
        let mut peer = WireGuardPeer::new(vec!["a:1".into()], now);
        peer.status = PeerStatus::Down;
        assert!(!peer.should_change_endpoint());
    }

    #[test]
    fn rotate_wraps_around() {
        let now = Instant::now();
        let mut peer = WireGuardPeer::new(vec!["a:1".into(), "b:2".into(), "c:3".into()], now);
        assert_eq!(peer.active_endpoint(), Some("a:1"));

        let t1 = now + Duration::from_secs(1);
        peer.rotate_endpoint(t1);
        assert_eq!(peer.active_endpoint(), Some("b:2"));
        assert_eq!(peer.last_endpoint_change, t1);
        assert_eq!(peer.status, PeerStatus::Unknown);

        peer.rotate_endpoint(t1 + Duration::from_secs(1));
        assert_eq!(peer.active_endpoint(), Some("c:3"));

        peer.rotate_endpoint(t1 + Duration::from_secs(2));
        assert_eq!(peer.active_endpoint(), Some("a:1"));
    }

    #[test]
    fn update_endpoints_preserves_active() {
        let now = Instant::now();
        let mut peer = WireGuardPeer::new(vec!["a:1".into(), "b:2".into(), "c:3".into()], now);
        peer.active_endpoint = 1; // "b:2"

        peer.update_endpoints(vec!["c:3".into(), "b:2".into(), "d:4".into()]);
        assert_eq!(peer.active_endpoint(), Some("b:2"));
        assert_eq!(peer.active_endpoint, 1);
    }

    #[test]
    fn update_endpoints_resets_when_active_removed() {
        let now = Instant::now();
        let mut peer = WireGuardPeer::new(vec!["a:1".into(), "b:2".into()], now);
        peer.active_endpoint = 1; // "b:2"

        peer.update_endpoints(vec!["c:3".into(), "d:4".into()]);
        assert_eq!(peer.active_endpoint, 0);
        assert_eq!(peer.active_endpoint(), Some("c:3"));
    }

    #[test]
    fn update_endpoints_handles_empty() {
        let now = Instant::now();
        let mut peer = WireGuardPeer::new(vec!["a:1".into()], now);
        peer.update_endpoints(vec![]);
        assert!(peer.endpoints.is_empty());
        assert_eq!(peer.active_endpoint, 0);
        assert_eq!(peer.active_endpoint(), None);
    }
}
