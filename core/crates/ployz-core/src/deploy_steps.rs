//! A Deployment's steps: when each phase of its Deploy started and ended, so a slow one
//! shows in `ployz deployment show`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::deploy_progress::{RowPhase, RowState, ServerProgress};
use crate::{MachineId, ServiceName};

/// One phase of a Deploy, in the order a Deploy reaches them.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum StepName {
    Receiving,
    FetchingSource,
    Preparing,
    Uploading,
    StartingBuilder,
    Building,
    Planning,
    DeliveringImage,
    Starting,
    HealthCheck,
    SwitchingTraffic,
    StoppingOld,
}

impl StepName {
    pub const ALL: [Self; 12] = [
        Self::Receiving,
        Self::FetchingSource,
        Self::Preparing,
        Self::Uploading,
        Self::StartingBuilder,
        Self::Building,
        Self::Planning,
        Self::DeliveringImage,
        Self::Starting,
        Self::HealthCheck,
        Self::SwitchingTraffic,
        Self::StoppingOld,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Receiving => "receiving",
            Self::FetchingSource => "fetching_source",
            Self::Preparing => "preparing",
            Self::Uploading => "uploading",
            Self::StartingBuilder => "starting_builder",
            Self::Building => "building",
            Self::Planning => "planning",
            Self::DeliveringImage => "delivering_image",
            Self::Starting => "starting",
            Self::HealthCheck => "health_check",
            Self::SwitchingTraffic => "switching_traffic",
            Self::StoppingOld => "stopping_old",
        }
    }

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|step| step.as_str() == name)
    }

    /// The step a row's work is in, or none once it ended or while it compensates.
    #[must_use]
    pub const fn of(state: &RowState) -> Option<Self> {
        match state {
            RowState::Pending => Some(Self::Starting),
            RowState::Running { phase, .. } => match phase {
                RowPhase::Starting
                | RowPhase::CreatingContainer
                | RowPhase::StartingContainer
                | RowPhase::UpdatingContainer => Some(Self::Starting),
                RowPhase::WaitingForHealth | RowPhase::WaitingForHook => Some(Self::HealthCheck),
                RowPhase::WaitingForSwitch => Some(Self::SwitchingTraffic),
                RowPhase::StoppingContainer
                | RowPhase::RemovingContainer
                | RowPhase::RemovingVolume => Some(Self::StoppingOld),
                RowPhase::Compensating => None,
            },
            RowState::Completed
            | RowState::Failed { .. }
            | RowState::NotAttempted
            | RowState::Unknown => None,
        }
    }
}

impl std::fmt::Display for StepName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// When one step of a Deployment ran, for one Service or the whole Deploy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct DeploymentStep {
    pub name: StepName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub service: Option<ServiceName>,
    /// Unix milliseconds.
    pub started_at: i64,
    /// Unix milliseconds; none while it runs, or when the Deploy stopped in it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub ended_at: Option<i64>,
}

/// Now, in Unix milliseconds.
#[must_use]
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}

type Key = (StepName, Option<ServiceName>);

/// The steps a runner saw, and which changed since it last took them.
#[derive(Debug, Default)]
pub struct Steps {
    steps: BTreeMap<Key, DeploymentStep>,
    changed: BTreeSet<Key>,
    /// The step each Service's row on each Machine is in.
    rows: BTreeMap<(ServiceName, MachineId), StepName>,
    /// The Machines each Service's step is running on.
    occupied: BTreeMap<(StepName, ServiceName), BTreeSet<MachineId>>,
    /// Steps that failed while running, so they stay open whatever comes next.
    held: BTreeSet<Key>,
}

impl Steps {
    /// Start `name` at `at`. A step that ended runs again from its first start.
    pub fn start(&mut self, name: StepName, service: Option<&ServiceName>, at: i64) {
        let key = (name, service.cloned());
        match self.steps.get_mut(&key) {
            Some(step) if step.ended_at.is_none() => return,
            Some(step) => step.ended_at = None,
            None => {
                self.steps.insert(
                    key.clone(),
                    DeploymentStep {
                        name,
                        service: service.cloned(),
                        started_at: at,
                        ended_at: None,
                    },
                );
            }
        }
        self.changed.insert(key);
    }

    /// End `name` at `at`, if it is running and didn't fail.
    pub fn end(&mut self, name: StepName, service: Option<&ServiceName>, at: i64) {
        let key = (name, service.cloned());
        if self.held.contains(&key) {
            return;
        }
        if let Some(step) = self.steps.get_mut(&key)
            && step.ended_at.is_none()
        {
            step.ended_at = Some(at);
            self.changed.insert(key);
        }
    }

    /// Keep `name` open from now on if it is running: it failed, though the Deploy goes
    /// on, as when an old copy's removal fails and the copy is left running.
    pub fn hold(&mut self, name: StepName, service: Option<&ServiceName>) {
        let key = (name, service.cloned());
        if self
            .steps
            .get(&key)
            .is_some_and(|step| step.ended_at.is_none())
        {
            self.held.insert(key);
        }
    }

    /// When the last of `name`'s Services ended it, if any did.
    #[must_use]
    pub fn last_ended(&self, name: StepName) -> Option<i64> {
        self.steps
            .values()
            .filter(|step| step.name == name)
            .filter_map(|step| step.ended_at)
            .max()
    }

    /// Whether `name` ever started.
    #[must_use]
    pub fn started(&self, name: StepName, service: Option<&ServiceName>) -> bool {
        self.steps.contains_key(&(name, service.cloned()))
    }

    /// Move each changed row to the step its state is in. A Service's step starts when
    /// its first row enters it and ends when its last row leaves it. A failed row stays
    /// in its step, so the Deployment shows where it failed.
    pub fn rows(&mut self, changes: &[ServerProgress], at: i64) {
        for row in changes {
            if matches!(row.state, RowState::Failed { .. }) {
                continue;
            }
            let key = (row.service.clone(), row.machine);
            let next = StepName::of(&row.state);
            let last = self.rows.get(&key).copied();
            if last == next {
                continue;
            }
            if let Some(last) = last {
                self.rows.remove(&key);
                let place = (last, row.service.clone());
                let empty = self.occupied.get_mut(&place).is_some_and(|machines| {
                    machines.remove(&row.machine);
                    machines.is_empty()
                });
                if empty {
                    self.occupied.remove(&place);
                    self.end(last, Some(&row.service), at);
                }
            }
            if let Some(next) = next {
                self.rows.insert(key, next);
                self.occupied
                    .entry((next, row.service.clone()))
                    .or_default()
                    .insert(row.machine);
                self.start(next, Some(&row.service), at);
            }
        }
    }

    /// The steps that changed since the last call, emptying the record of changes.
    pub fn take_changed(&mut self) -> Vec<DeploymentStep> {
        std::mem::take(&mut self.changed)
            .into_iter()
            .filter_map(|key| self.steps.get(&key).cloned())
            .collect()
    }
}

#[cfg(test)]
#[path = "deploy_steps_tests.rs"]
mod tests;
