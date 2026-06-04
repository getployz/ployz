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
pub enum ScheduleCapability {
    Unsupported,
    MessageSchedules,
    ExtendedControls,
}

impl ScheduleCapability {
    #[must_use]
    pub fn from_server_version(version: NatsServerVersion) -> Self {
        if version >= NatsServerVersion::new(2, 14, 0) {
            Self::ExtendedControls
        } else if version >= NatsServerVersion::new(2, 12, 0) {
            Self::MessageSchedules
        } else {
            Self::Unsupported
        }
    }

    #[must_use]
    pub const fn message_schedules_available(self) -> bool {
        matches!(self, Self::MessageSchedules | Self::ExtendedControls)
    }

    #[must_use]
    pub const fn extended_schedule_controls_available(self) -> bool {
        matches!(self, Self::ExtendedControls)
    }
}
