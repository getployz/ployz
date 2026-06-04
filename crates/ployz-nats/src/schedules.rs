//! Message schedule capability detection and helpers.

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct NatsServerVersion {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}

impl NatsServerVersion {
    #[must_use]
    pub const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduleCapability {
    pub message_schedules_available: bool,
    pub extended_schedule_controls_available: bool,
}

impl ScheduleCapability {
    #[must_use]
    pub fn from_server_version(version: NatsServerVersion) -> Self {
        Self {
            message_schedules_available: version >= NatsServerVersion::new(2, 12, 0),
            extended_schedule_controls_available: version >= NatsServerVersion::new(2, 14, 0),
        }
    }
}
