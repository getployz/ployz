use std::time::{SystemTime, UNIX_EPOCH};

#[must_use]
pub fn now_unix_secs() -> u64 {
    unix_secs_from_system_time(SystemTime::now())
}

fn unix_secs_from_system_time(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    #[test]
    fn now_unix_secs_returns_nonzero_current_time() {
        assert!(now_unix_secs() > 0);
    }

    #[test]
    fn unix_secs_from_system_time_returns_zero_before_epoch() {
        assert_eq!(
            unix_secs_from_system_time(UNIX_EPOCH - Duration::from_secs(1)),
            0
        );
    }
}
