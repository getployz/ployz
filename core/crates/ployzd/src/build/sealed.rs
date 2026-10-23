//! Opening a Build's Sealed values on this Machine and hiding them in
//! everything the Build reports back.

use std::{collections::BTreeMap, path::Path, sync::Mutex};

use ployz_build::{Progress, remote::Outcome};
use ployz_core::scrub::{Channel, FrameScrub, SealedForms};

use crate::sealed_env::SealedRecord;

/// The private file a capture writes its Sealed values to, still sealed.
pub(crate) const RECORD: &str = "sealed.json";

pub(crate) struct Opened {
    pub(crate) values: BTreeMap<String, String>,
    pub(crate) scrub: OutputScrub,
}

/// Open the record with this Machine's Sealing Key. The message names a
/// variable at most, never what it holds.
pub(crate) fn open(record: &[u8], key_dir: Option<&Path>) -> Result<Opened, String> {
    let record: SealedRecord = serde_json::from_slice(record)
        .map_err(|_| "the Build's sealed values are not a valid record".to_owned())?;
    let (values, forms) = record
        .open_for_build(key_dir)
        .map_err(|error| error.to_string())?;
    Ok(Opened {
        values,
        scrub: OutputScrub {
            forms,
            frames: Mutex::new(FrameScrub::new()),
        },
    })
}

pub(crate) struct OutputScrub {
    forms: SealedForms,
    frames: Mutex<FrameScrub>,
}

impl OutputScrub {
    pub(crate) fn progress(&self, progress: Progress) -> Progress {
        let mut redacted = 0;
        match progress {
            Progress::Output(bytes) => {
                Progress::Output(self.frames.lock().expect("Build output scrub lock").push(
                    Channel::Stderr,
                    &bytes,
                    &self.forms,
                ))
            }
            Progress::Step(mut step) => {
                step.name = self.forms.scrub(&step.name, &mut redacted);
                step.error = step
                    .error
                    .map(|error| self.forms.scrub(&error, &mut redacted));
                Progress::Step(step)
            }
            Progress::StepOutput { step, stderr, text } => Progress::StepOutput {
                step,
                stderr,
                text: self.forms.scrub(&text, &mut redacted),
            },
            other @ (Progress::Stage(_) | Progress::Timing { .. } | Progress::Target { .. }) => {
                other
            }
        }
    }

    /// What the stream still held when the Build ended.
    pub(crate) fn finish(&self) -> Option<Progress> {
        let frames = std::mem::take(&mut *self.frames.lock().expect("Build output scrub lock"));
        let tail: Vec<u8> = frames
            .finish(&self.forms)
            .into_iter()
            .flat_map(|(_, bytes)| bytes)
            .collect();
        (!tail.is_empty()).then_some(Progress::Output(tail))
    }

    pub(crate) fn outcome(&self, outcome: Outcome) -> Outcome {
        let mut redacted = 0;
        match outcome {
            Outcome::Failed {
                work,
                stage,
                message,
            } => Outcome::Failed {
                work,
                stage,
                message: self.forms.scrub(&message, &mut redacted),
            },
            Outcome::Unknown {
                work,
                stage,
                message,
            } => Outcome::Unknown {
                work,
                stage,
                message: self.forms.scrub(&message, &mut redacted),
            },
            other @ (Outcome::Images { .. }
            | Outcome::Validated { .. }
            | Outcome::Published { .. }
            | Outcome::CapabilitiesChecked { .. }) => other,
        }
    }
}
