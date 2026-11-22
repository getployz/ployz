use ployz_core::Region;
use ployz_core::{Hint, InitialMachinePolicy, Machine, MachineLabelKey, MachineLabelValue};
use ployz_store::region_key;

use crate::failure::Failure as Error;

/// The policy the `server add` flags ask for. The region stays out of the labels until the
/// Server is known: a new Server starts in the region asked, else in default, and a Server
/// that already exists keeps its own.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AskedPolicy {
    /// Never holds the `region` label.
    policy: InitialMachinePolicy,
    region: Option<Region>,
}

impl AskedPolicy {
    /// The labels, `--region`, and the `--accepts-builds`, `--accepts-services` and
    /// `--accepts-ingress` values given, each on unless said off.
    pub(crate) fn parse(
        labels: &[String],
        region: Option<&str>,
        accepts: [Option<bool>; 3],
    ) -> Result<Self, Error> {
        let mut labels = parse_labels(labels)?;
        let label = labels
            .remove(&region_label()?)
            .map(|label| region_key(label.as_str()))
            .transpose()?;
        let flag = region.map(region_key).transpose()?;
        let region = match (flag, label) {
            (Some(flag), Some(label)) if flag != label => {
                return Err(Error::usage(format!(
                    "--region {flag} and --label-add region={label} disagree. Keep --region {flag} and drop the region label."
                )));
            }
            (flag, label) => flag.or(label),
        };
        Ok(Self {
            policy: InitialMachinePolicy {
                labels,
                accepts_builds: accepts[0].unwrap_or(true),
                accepts_services: accepts[1].unwrap_or(true),
                accepts_ingress: accepts[2].unwrap_or(true),
            },
            region,
        })
    }

    /// The policy of the Server this enrollment lands on: `existing`, or a new Server when
    /// `None`. Enrollment never edits a Server, so a region asked for one already elsewhere
    /// is refused, naming the command `rerun` builds to move it.
    pub(crate) fn settle(
        &self,
        existing: Option<&Machine>,
        rerun: impl Fn(&[&str]) -> String,
    ) -> Result<InitialMachinePolicy, Error> {
        let key = region_label()?;
        let region = match existing {
            None => Some(self.region.clone().unwrap_or_default().as_str().parse()?),
            Some(server) => {
                let held = server.labels.get(&key);
                if let Some(asked) = &self.region
                    && !held.is_some_and(|held| {
                        region_key(held.as_str()).is_ok_and(|held| held == *asked)
                    })
                {
                    return Err(elsewhere(server, held, asked, rerun));
                }
                held.cloned()
            }
        };
        let mut policy = self.policy.clone();
        policy.labels.extend(region.map(|region| (key, region)));
        Ok(policy)
    }
}

fn region_label() -> Result<MachineLabelKey, Error> {
    Ok(Region::LABEL.parse()?)
}

fn elsewhere(
    server: &Machine,
    held: Option<&MachineLabelValue>,
    asked: &Region,
    rerun: impl Fn(&[&str]) -> String,
) -> Error {
    let name = &server.name;
    let now = held.map_or_else(
        || "has no region".to_owned(),
        |held| format!("is in region {held}"),
    );
    let label = format!("{}={asked}", Region::LABEL);
    Error::conflict(format!(
        "Server {name} {now}, not {asked}. Adding it again doesn't move it."
    ))
    .hint(Hint::Next(rerun(&[
        "server",
        "set",
        name.as_str(),
        "--label-add",
        &label,
    ])))
}

pub(crate) fn parse_labels(
    given: &[String],
) -> Result<
    std::collections::BTreeMap<ployz_core::MachineLabelKey, ployz_core::MachineLabelValue>,
    Error,
> {
    let mut labels = std::collections::BTreeMap::new();
    for label in given {
        let (key, value) = label
            .split_once('=')
            .ok_or_else(|| Error::usage(format!("invalid label {label:?}: expected KEY=VALUE")))?;
        if labels.insert(key.parse()?, value.parse()?).is_some() {
            return Err(Error::usage(format!("duplicate label key {key:?}")));
        }
    }
    Ok(labels)
}
