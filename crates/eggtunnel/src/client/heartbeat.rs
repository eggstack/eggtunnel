//! Bounded heartbeat state owned by one authenticated Session.

use tokio::time::Instant;

pub(super) struct HeartbeatState {
    nonce: u64,
    outstanding: Option<(u64, Instant)>,
}

impl HeartbeatState {
    pub fn new() -> Self {
        Self {
            nonce: 0,
            outstanding: None,
        }
    }

    pub fn has_outstanding(&self) -> bool {
        self.outstanding.is_some()
    }

    pub fn next_nonce(&mut self) -> u64 {
        self.nonce = self.nonce.wrapping_add(1);
        self.nonce
    }

    pub fn mark_sent(&mut self, nonce: u64, sent_at: Instant) {
        self.outstanding = Some((nonce, sent_at));
    }

    pub fn matching_pong(&mut self, nonce: u64) -> Option<Instant> {
        if self
            .outstanding
            .is_some_and(|(expected, _)| expected == nonce)
        {
            self.outstanding.take().map(|(_, sent_at)| sent_at)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_the_outstanding_probe_and_new_state_resets_generation() {
        let sent = Instant::now();
        let mut first_session = HeartbeatState::new();
        let nonce = first_session.next_nonce();
        first_session.mark_sent(nonce, sent);
        assert!(first_session.has_outstanding());
        assert!(first_session.matching_pong(nonce.wrapping_add(1)).is_none());
        assert_eq!(first_session.matching_pong(nonce), Some(sent));
        assert!(!first_session.has_outstanding());

        let next_session = HeartbeatState::new();
        assert!(!next_session.has_outstanding());
    }
}
