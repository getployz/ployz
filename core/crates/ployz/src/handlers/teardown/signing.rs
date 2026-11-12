//! A Device's teardown. When a Server of the Cluster enforces Device approval, `ployz env rm`,
//! `ployz project rm` and a `ployz deploy` that accepts losing Volumes sign what they take off
//! with this CLI's Device key: the Namespaces, the Servers and each Volume whose data goes.
//! Each Server then takes off only what the person read.

use clap::ArgMatches;
use ployz_core::authority::shown::Shown;
use ployz_core::authority::{
    Signed, Signer, SignerKey, Teardown, TeardownApproval, TeardownScope, TeardownTerms, Timestamp,
};
use ployz_core::{DeviceKey, DockerVolumeName, Hint, MachineId, RpcErrorCode};

use super::super::Error;
use super::super::deploy::approve::{Probe, probe};

/// A teardown drafted before the person says yes, so its lines show beside the question. It
/// is stamped when signed, so its window starts at the yes.
pub(in crate::handlers) struct Drafted {
    terms: TeardownTerms,
    signer: Signer,
    lines: Vec<String>,
}

/// Draft the teardown of `scope`, given every Server of the Cluster, when this CLI holds a
/// Device key and a Server enforces Device approval. `label` names each Docker Volume as the
/// person knows it. None when there is nothing to sign: no Device key, or no Server
/// enforcing. `what` names the change in the warning when no Server answered.
///
/// # Errors
/// Refuses when a Server enforces but none reports the Cluster's key; returns the key's
/// error.
pub(in crate::handlers) fn draft(
    matches: &ArgMatches,
    what: &str,
    scope: impl FnOnce(&[MachineId]) -> TeardownScope,
    label: impl Fn(&DockerVolumeName) -> String,
) -> Result<Option<Drafted>, Error> {
    let key = super::super::device::key::path(&super::super::config_path(matches)?);
    let Ok(Some(secret)) = super::super::device::key::load(&key) else {
        return Ok(None);
    };
    let enforcing = match probe(matches) {
        Probe::Off => return Ok(None),
        Probe::Unknown => {
            crate::ui::warn(format!(
                "Couldn't ask your servers whether they enforce device approval, so this \
                 device didn't approve {what}. Servers that enforce will refuse it."
            ));
            return Ok(None);
        }
        Probe::NoClusterKey => {
            return Err(Error::coded(
                RpcErrorCode::Unavailable,
                format!(
                    "Your servers enforce device approval, but none reported the cluster's \
                     key, so this device can't approve {what}. Run ployz status, then try \
                     again."
                ),
            ));
        }
        Probe::Enforcing(enforcing) => enforcing,
    };
    let signer = Signer::from_seed(&secret);
    let terms = TeardownTerms {
        cluster: enforcing.cluster,
        scope: scope(&enforcing.machines),
        issued_at: Timestamp::now(),
        signer: SignerKey::Device(signer.public().into()),
    };
    let name = |id: &MachineId| {
        enforcing
            .names
            .get(id)
            .cloned()
            .unwrap_or_else(|| id.to_string())
    };
    let mut lines = Teardown::drafted(terms.clone()).lines(name, label);
    // The rest of what it signs. The window starts at the yes, when `sign` stamps it.
    lines.extend([
        Shown::words("  cluster ")
            .and_text(&terms.cluster.to_string())
            .into(),
        Shown::words("  signer device ")
            .and_text(&DeviceKey::from(signer.public()).code().to_string())
            .into(),
        "  valid for 15 minutes from your yes".to_owned(),
    ]);
    Ok(Some(Drafted {
        terms,
        signer,
        lines,
    }))
}

impl Drafted {
    /// Show the lines this Device signs, on stderr beside the question.
    pub(in crate::handlers) fn show(&self) {
        for line in &self.lines {
            crate::ui::note(format_args!("{line}"));
        }
    }

    /// Ask the person to sign the lines shown, unless `--yes` answered; a run that can't ask
    /// refuses before signing. `--yes` names the flag the retry adds.
    ///
    /// # Errors
    /// Refuses when the person declines, or no terminal can ask and `--yes` wasn't given.
    pub(in crate::handlers) fn ask(
        &self,
        matches: &ArgMatches,
        question: &str,
    ) -> Result<(), Error> {
        let yes = matches
            .try_get_one::<bool>("yes")
            .ok()
            .flatten()
            .copied()
            .unwrap_or(false);
        consent(yes, question, |question, refusal, declined| {
            crate::ui::confirm(question, || refusal, declined)
        })
    }

    /// Sign the teardown now, its window starting from this moment.
    ///
    /// # Errors
    /// Returns the key's error.
    pub(in crate::handlers) fn sign(self) -> Result<TeardownApproval, Error> {
        let body = Teardown::drafted(TeardownTerms {
            issued_at: Timestamp::now(),
            ..self.terms
        });
        Signed::sign(&body, &self.signer)
            .map(TeardownApproval::Signed)
            .map_err(Error::command)
    }

    #[cfg(test)]
    pub(in crate::handlers) fn lines(&self) -> &[String] {
        &self.lines
    }
}

/// What a person says to signing a teardown, asked after its lines show. `--yes` answers for
/// a run nobody watches; without it, a run that can't ask refuses before signing.
fn consent(
    yes: bool,
    question: &str,
    confirm: impl FnOnce(&str, Error, &'static str) -> Result<(), Error>,
) -> Result<(), Error> {
    if yes {
        return Ok(());
    }
    let refusal = Error::coded(
        RpcErrorCode::ConfirmationRequired,
        "Your servers enforce device approval, so deleting this data needs your yes, and \
         there's no terminal to ask in. Read the lines above, then add --yes to sign them. \
         Nothing deleted.",
    )
    .hint(Hint::Retry(super::super::typed_with(&["--yes"])));
    confirm(question, refusal, "Cancelled. Nothing signed or deleted.")
}

#[cfg(test)]
#[path = "signing_tests.rs"]
mod tests;
