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
        scrub: OutputScrub::new(forms),
    })
}

pub(crate) struct OutputScrub {
    forms: SealedForms,
    frames: Mutex<Frames>,
}

/// One stream scrubber for the unattributed output and one per step, since a
/// value never continues from one step's output into another's.
#[derive(Default)]
struct Frames {
    output: FrameScrub,
    steps: BTreeMap<String, FrameScrub>,
}

const fn channel(stderr: bool) -> Channel {
    if stderr {
        Channel::Stderr
    } else {
        Channel::Stdout
    }
}

impl OutputScrub {
    pub(crate) fn new(forms: SealedForms) -> Self {
        Self {
            forms,
            frames: Mutex::new(Frames::default()),
        }
    }

    /// What of `progress` can be reported now, in order. Output is held back
    /// where it could begin a value, so one event may become none, and a step
    /// completing releases what its output still held before the step itself.
    pub(crate) fn progress(&self, progress: Progress) -> Vec<Progress> {
        let mut redacted = 0;
        match progress {
            Progress::Output(bytes) => {
                let shown = self
                    .frames
                    .lock()
                    .expect("Build output scrub lock")
                    .output
                    .push(Channel::Stderr, &bytes, &self.forms);
                if shown.is_empty() {
                    Vec::new()
                } else {
                    vec![Progress::Output(shown)]
                }
            }
            Progress::Step(mut step) => {
                step.name = self.forms.scrub(&step.name, &mut redacted);
                step.error = step
                    .error
                    .map(|error| self.forms.scrub(&error, &mut redacted));
                let mut events = Vec::new();
                if step.completed.is_some() {
                    let frames = self
                        .frames
                        .lock()
                        .expect("Build output scrub lock")
                        .steps
                        .remove(&step.id);
                    if let Some(frames) = frames {
                        events.extend(self.step_tails(&step.id, frames));
                    }
                }
                events.push(Progress::Step(step));
                events
            }
            Progress::StepOutput { step, stderr, text } => {
                let shown = self
                    .frames
                    .lock()
                    .expect("Build output scrub lock")
                    .steps
                    .entry(step.clone())
                    .or_default()
                    .push(channel(stderr), text.as_bytes(), &self.forms);
                if shown.is_empty() {
                    Vec::new()
                } else {
                    vec![Progress::StepOutput {
                        step,
                        stderr,
                        text: String::from_utf8_lossy(&shown).into_owned(),
                    }]
                }
            }
            other @ (Progress::Stage(_) | Progress::Timing { .. } | Progress::Target { .. }) => {
                vec![other]
            }
        }
    }

    /// What every stream still held when the Build ended, with any trailing
    /// start of a value hidden. A Build that ends early, cancelled or failed,
    /// reaches this the same way as one that completes.
    pub(crate) fn finish(&self) -> Vec<Progress> {
        let frames = std::mem::take(&mut *self.frames.lock().expect("Build output scrub lock"));
        let mut events: Vec<Progress> = frames
            .steps
            .into_iter()
            .flat_map(|(step, frames)| self.step_tails(&step, frames))
            .collect();
        let tail: Vec<u8> = frames
            .output
            .finish(&self.forms)
            .into_iter()
            .flat_map(|(_, bytes)| bytes)
            .collect();
        if !tail.is_empty() {
            events.push(Progress::Output(tail));
        }
        events
    }

    fn step_tails(&self, step: &str, frames: FrameScrub) -> Vec<Progress> {
        frames
            .finish(&self.forms)
            .into_iter()
            .map(|(channel, bytes)| Progress::StepOutput {
                step: step.to_owned(),
                stderr: matches!(channel, Channel::Stderr),
                text: String::from_utf8_lossy(&bytes).into_owned(),
            })
            .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_build::BuildStep;
    use ployz_core::scrub::SealedValues;

    const STEP: &str = "sha256:step-one";

    fn scrub(value: &str) -> OutputScrub {
        let values: SealedValues = [("NPM_TOKEN".to_owned(), value.to_owned())]
            .into_iter()
            .collect();
        OutputScrub::new(SealedForms::compile(&values))
    }

    fn step_output(text: &str) -> Progress {
        Progress::StepOutput {
            step: STEP.into(),
            stderr: false,
            text: text.into(),
        }
    }

    fn completed() -> Progress {
        Progress::Step(BuildStep {
            id: STEP.into(),
            name: "RUN npm install".into(),
            started: Some("t0".into()),
            completed: Some("t1".into()),
            cached: false,
            error: None,
        })
    }

    /// Every event's text in order, with the shown step output joined.
    fn report(scrub: &OutputScrub, events: Vec<Progress>) -> (String, Vec<Progress>) {
        let mut shown = Vec::new();
        for event in events {
            shown.extend(scrub.progress(event));
        }
        shown.extend(scrub.finish());
        let text = shown
            .iter()
            .filter_map(|event| match event {
                Progress::StepOutput { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        (text, shown)
    }

    #[test]
    fn a_value_cut_between_two_step_output_events_never_shows() {
        let value = "opened-token-0123456789";
        let scrub = scrub(value);
        let padding = "x".repeat(64 * 1024 - 7);
        let first = format!("{padding}{}", &value[..7]);
        let second = format!("{} and more\n", &value[7..]);
        let (text, shown) = report(
            &scrub,
            vec![step_output(&first), step_output(&second), completed()],
        );
        assert!(!text.contains(value), "{text}");
        assert_eq!(text, format!("{padding}[secret NPM_TOKEN] and more\n"));
        assert!(matches!(shown.last(), Some(Progress::Step(step)) if step.completed.is_some()));
    }

    #[test]
    fn a_multiline_value_cut_at_its_own_newline_never_shows() {
        let value = "line1-secret\nline2-secret";
        let scrub = scrub(value);
        let (text, _) = report(
            &scrub,
            vec![
                step_output("cert: line1-secret\n"),
                step_output("line2-secret end\n"),
                completed(),
            ],
        );
        assert!(!text.contains("line1-secret"), "{text}");
        assert!(!text.contains("line2-secret"), "{text}");
        assert_eq!(text, "cert: [secret NPM_TOKEN] end\n");
    }

    #[test]
    fn plain_output_arrives_whole_and_in_order_and_a_held_tail_leaves_at_the_end() {
        let scrub = scrub("opened-token-0123456789");
        let events = vec![
            step_output("one\n"),
            step_output("two "),
            step_output("three\n"),
            Progress::StepOutput {
                step: "sha256:step-two".into(),
                stderr: true,
                text: "warning: slow".into(),
            },
            step_output("four"),
        ];
        let (text, shown) = report(&scrub, events);
        assert_eq!(text, "one\ntwo three\nfourwarning: slow");
        let order: Vec<(&str, bool)> = shown
            .iter()
            .filter_map(|event| match event {
                Progress::StepOutput { step, stderr, .. } => Some((step.as_str(), *stderr)),
                _ => None,
            })
            .collect();
        // Held text leaves as late as it must: "two " waits for its line end, and
        // what each step still holds leaves at the end, step by step.
        assert_eq!(
            order,
            [
                (STEP, false),
                (STEP, false),
                (STEP, false),
                ("sha256:step-two", true)
            ]
        );
    }

    #[test]
    fn a_value_begun_when_the_build_stops_stays_hidden() {
        let value = "opened-token-0123456789";
        let scrub = scrub(value);
        let (text, _) = report(&scrub, vec![step_output("token is opened-tok")]);
        assert!(!text.contains("opened-tok"), "{text}");
        assert_eq!(text, "token is [secret NPM_TOKEN]");
    }
}
