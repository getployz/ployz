#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrepareVote {
    Allow,
    RetryableDeny,
    TerminalDeny,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuorumDecision {
    pub allowed: bool,
    pub votes_for: usize,
    pub votes_total: usize,
    pub retryable_denied: usize,
}

impl QuorumDecision {
    #[must_use]
    pub fn retry_could_succeed(&self) -> bool {
        self.votes_for + self.retryable_denied > self.votes_total / 2
    }
}

pub async fn quorum_prepare<P, F, Fut>(
    peers: &[P],
    local_vote: PrepareVote,
    remote: F,
) -> QuorumDecision
where
    P: Clone,
    F: Fn(P) -> Fut,
    Fut: std::future::Future<Output = PrepareVote>,
{
    let mut votes_for = usize::from(matches!(local_vote, PrepareVote::Allow));
    let mut retryable_denied = usize::from(matches!(local_vote, PrepareVote::RetryableDeny));
    for peer in peers {
        match remote(peer.clone()).await {
            PrepareVote::Allow => votes_for += 1,
            PrepareVote::RetryableDeny => retryable_denied += 1,
            PrepareVote::TerminalDeny => {}
        }
    }
    let votes_total = peers.len() + 1;
    QuorumDecision {
        allowed: votes_for > votes_total / 2,
        votes_for,
        votes_total,
        retryable_denied,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::model::MachineId;

    #[tokio::test]
    async fn quorum_prepare_requires_strict_majority() {
        let peers = vec![MachineId("peer-a".into()), MachineId("peer-b".into())];

        let denied = quorum_prepare(&peers, PrepareVote::Allow, |_| async {
            PrepareVote::TerminalDeny
        })
        .await;
        assert!(!denied.allowed);
        assert_eq!(denied.votes_for, 1);
        assert_eq!(denied.votes_total, 3);
        assert!(!denied.retry_could_succeed());

        let allowed = quorum_prepare(&peers, PrepareVote::Allow, |peer| async move {
            match peer.0.as_str() {
                "peer-a" => PrepareVote::Allow,
                "peer-b" => PrepareVote::RetryableDeny,
                _ => unreachable!("unexpected peer"),
            }
        })
        .await;
        assert!(allowed.allowed);
        assert_eq!(allowed.votes_for, 2);
        assert_eq!(allowed.votes_total, 3);
        assert!(allowed.retry_could_succeed());
    }

    #[tokio::test]
    async fn quorum_prepare_tracks_retryable_denials_separately() {
        let peers = vec![
            MachineId("peer-a".into()),
            MachineId("peer-b".into()),
            MachineId("peer-c".into()),
        ];

        let denied = quorum_prepare(&peers, PrepareVote::Allow, |peer| async move {
            match peer.0.as_str() {
                "peer-a" => PrepareVote::RetryableDeny,
                "peer-b" => PrepareVote::TerminalDeny,
                "peer-c" => PrepareVote::TerminalDeny,
                _ => unreachable!("unexpected peer"),
            }
        })
        .await;
        assert!(!denied.allowed);
        assert_eq!(denied.votes_for, 1);
        assert_eq!(denied.retryable_denied, 1);
        assert!(!denied.retry_could_succeed());
    }
}
