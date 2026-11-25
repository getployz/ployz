//! Seals a write's new secrets and registry logins in this CLI when the write goes
//! through Cloud. A login is sealed for a registry the person names: the image the
//! same write sets, `--registry`, or the image Cloud names once they say yes, never
//! Cloud's word alone. Cloud says whether it does and to which key, asked once per command and only for
//! a write that carries plain text; the Store says where each secret lands. The key is
//! sealed to only when it is the one this computer pinned for the Organization, and a
//! secret's plain text never goes to Cloud. The in-process Store seals every secret itself.

use ployz_core::{
    Hint, RpcErrorCode, SealedIdentity, SealingPublicKey, ServiceName, UppercaseHost,
    image_registry, is_image_reference, login_host, seal_login, seal_value,
};
use ployz_store::{
    Ask as _, Edit, EnvironmentSummary, SealingService, SealingTargetsQuery, SealingTargetsView,
    SecretValue, SyncChanges, View,
};
use serde::Deserialize;
use uuid::Uuid;

use super::super::config_path;
use super::{Backend, Error, Store};
use crate::cloud;
use crate::cloud::account::StoreCallError;
use crate::sealing_pin::{self, Pins};
use crate::ui;

/// What Cloud answers about sealing one write's secrets.
#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum Sealing {
    /// The Organization seals in the Store, as a Cloud from before sealing on the client
    /// says. The CLI never sends it a secret's plain text.
    Off,
    /// It seals on the client, but no Server holds a Sealing Key yet.
    #[serde(rename_all = "camelCase")]
    NoServer {
        /// A Server awaits `ployz recover`. Absent from a Cloud before 0.3.10.
        #[serde(default)]
        awaits_recovery: bool,
    },
    /// It seals on the client, to this key, and the write's secrets land here.
    #[serde(rename_all = "camelCase")]
    Ready {
        public_key: SealingPublicKey,
        targets: Box<View>,
    },
}

impl Backend {
    /// What Cloud answers about sealing `query`'s secrets; None for the in-process Store,
    /// which seals every secret itself.
    fn sealing(&self, query: &SealingTargetsQuery) -> Result<Option<Sealing>, StoreCallError> {
        let Self::Cloud(runtime, credential) = self else {
            return Ok(None);
        };
        let answer = runtime.block_on(cloud::account::sealing(
            credential,
            cloud::http::Via::Cli,
            query,
        ))?;
        // A Cloud without the route predates sealing on the client, as one answering Off does.
        Ok(Some(answer.unwrap_or(Sealing::Off)))
    }
}

impl Store<'_> {
    /// `edit` with each new secret's plain text sealed for the Service it sets, when it
    /// goes through Cloud. `fail` maps what Cloud or the Store
    /// refuses, as it maps the write's own refusal.
    ///
    /// # Errors
    /// `conflict` when the Organization has no Server, or Cloud's key isn't the one this
    /// computer pinned; `unsupported` when Cloud offers no key; what Cloud or the Store
    /// refused, the text included.
    pub(crate) fn seal_edit(
        &self,
        mut edit: Edit,
        registry: Option<&str>,
        fail: impl Fn(StoreCallError) -> Error,
    ) -> Result<Edit, Error> {
        let refused = |error| fail(StoreCallError::Refused(error));
        // A login this CLI can't read is refused here, so its text never leaves.
        let logins = edit.carries_plaintext_login().map_err(refused)?;
        if !edit.carries_plaintext_secret() && !logins {
            return Ok(edit);
        }
        let query = SealingTargetsQuery::Edit {
            environment: edit.environment.clone(),
        };
        let Some((recipient, targets)) = self.ready(&query, &fail)? else {
            return Ok(edit);
        };
        let SealingTargetsView::Edit {
            organization,
            environment,
            services,
        } = targets
        else {
            return Err(unexpected("an Edit's targets"));
        };
        let environment_id = bound(&environment)?;
        edit.map_plaintext_secrets(refused, |secret| {
            let service =
                SealingService::bind(&services, &environment, secret.service(), secret.key())
                    .map_err(refused)?;
            let identity = SealedIdentity {
                organization: organization.clone(),
                environment: environment_id,
                service,
            };
            let plaintext = secret.admit().map_err(refused)?;
            seal(&recipient, &identity, secret.key(), plaintext).map(Some)
        })?;
        edit.map_plaintext_logins(refused, |service, set, login| {
            let named = services
                .iter()
                .find(|target| &target.name == service)
                .and_then(|target| target.image.as_deref());
            let host = login_host_for(service, set, registry, named)?;
            seal_login(&recipient, &host, &login).map_err(|error| {
                Error::caused(
                    RpcErrorCode::Internal,
                    format!("{service}.registryCredential: could not seal the credentials"),
                    error,
                )
            })
        })?;
        Ok(edit)
    }

    /// `request` with each plain-text value sealed for the receiver's Service its row
    /// lands in, when it goes through Cloud. `fail` maps what Cloud or
    /// the Store refuses, as it maps the write's own refusal.
    ///
    /// # Errors
    /// As [`Self::seal_edit`].
    pub(crate) fn seal_sync_values(
        &self,
        mut request: SyncChanges,
        fail: impl Fn(StoreCallError) -> Error,
    ) -> Result<SyncChanges, Error> {
        let rows: Vec<_> = request
            .values
            .iter()
            .filter(|(_, value)| matches!(value, SecretValue::Text(_)))
            .map(|(asked, _)| asked.clone())
            .collect();
        if rows.is_empty() {
            return Ok(request);
        }
        let query = SealingTargetsQuery::Sync {
            from: request.from.clone(),
            into: request.into.clone(),
            version: request.version.clone(),
            rows,
        };
        let Some((recipient, targets)) = self.ready(&query, &fail)? else {
            return Ok(request);
        };
        let SealingTargetsView::Sync {
            organization,
            into,
            values: targets,
            ..
        } = targets
        else {
            return Err(unexpected("a Sync's targets"));
        };
        let environment = bound(&into)?;
        for (asked, value) in &mut request.values {
            let SecretValue::Text(plaintext) = value else {
                continue;
            };
            let Some(target) = targets.iter().find(|target| &target.asked == asked) else {
                return Err(unexpected(&format!("where {asked} lands")));
            };
            let identity = SealedIdentity {
                organization: organization.clone(),
                environment,
                service: target.service,
            };
            let plaintext = target
                .admit(plaintext)
                .map_err(|error| fail(StoreCallError::Refused(error)))?;
            *value =
                SecretValue::ClientSealed(seal(&recipient, &identity, &target.key, plaintext)?);
        }
        Ok(request)
    }

    /// The key to seal to and where `query`'s secrets land; `None` when the Store seals.
    /// Cloud's key counts only when it is the one pinned for the Organization it names.
    fn ready(
        &self,
        query: &SealingTargetsQuery,
        fail: impl Fn(StoreCallError) -> Error,
    ) -> Result<Option<(SealingPublicKey, SealingTargetsView)>, Error> {
        match self.backend.sealing(query).map_err(&fail)? {
            None => Ok(None),
            Some(Sealing::Off) => Err(Error::coded(
                RpcErrorCode::Unsupported,
                "Ployz Cloud offered no key to seal this secret to on your computer, so nothing \
                 was sent. If you host Ployz Cloud yourself, update it.",
            )),
            Some(Sealing::NoServer {
                awaits_recovery: true,
            }) => Err(Error::conflict(
                // Cloud learns of a recover from the Server's next report, a few seconds later.
                "Your server needs the Sealing Key back before it can take sealed values. \
                 If you just ran ployz recover, try again in a few seconds.",
            )
            .hint(Hint::Next("sudo ployz recover".into()))),
            Some(Sealing::NoServer {
                awaits_recovery: false,
            }) => Err(Error::conflict("Sealed values need a server.")
                .hint(Hint::Next("ployz server add".into()))),
            Some(Sealing::Ready {
                public_key,
                targets,
            }) => {
                let targets = SealingTargetsQuery::view(*targets)
                    .map_err(|error| fail(StoreCallError::Refused(error)))?;
                let (SealingTargetsView::Edit { organization, .. }
                | SealingTargetsView::Sync { organization, .. }) = &targets;
                let pin = Pins::beside(&config_path(self.matches)?).get(organization)?;
                Ok(Some((
                    sealing_pin::check(pin.as_ref(), public_key)?,
                    targets,
                )))
            }
        }
    }
}

/// The registry host to seal `service`'s login for: that of `set`, the image this edit
/// sets, or `registry`, from `--registry`. Otherwise that of `named`, the image Cloud
/// says `service` runs, once the person at the terminal says yes: Cloud alone never
/// chooses where a token goes.
fn login_host_for(
    service: &ServiceName,
    set: Option<&str>,
    registry: Option<&str>,
    named: Option<&str>,
) -> Result<String, Error> {
    let path = format!("{service}.registryCredential");
    let host_of = |image: &str| {
        login_host(image)
            .map(str::to_owned)
            .map_err(|error| unreadable_host(&path, error))
    };
    let registry = registry
        .map(|host| match image_registry(&format!("{host}/image")) {
            Some(first) if first == host => host_of(&format!("{host}/image")),
            Some(_) | None => Err(Error::usage(format!(
                "--registry takes a registry host, like ghcr.io or registry.example.com:5000, not {host}."
            ))),
        })
        .transpose()?;
    if let Some(image) = set {
        let host = host_of(image)?;
        return match registry {
            Some(registry) if registry != host => Err(Error::usage(format!(
                "{path}: the image is on {host}, not {registry}."
            ))),
            Some(_) | None => Ok(host),
        };
    }
    if let Some(registry) = registry {
        return Ok(registry);
    }
    let (host, question) = named_host(service, named)?;
    ui::confirm(
        &question,
        || {
            Error::usage(format!(
                "{path}: name the registry these credentials are for, like --registry ghcr.io."
            ))
        },
        "Cancelled. Nothing sent.",
    )?;
    Ok(host)
}

/// A registry no login can be sealed for as written, with why as its cause.
fn unreadable_host(path: &str, error: UppercaseHost) -> Error {
    Error::caused(
        RpcErrorCode::InvalidArgument,
        format!("{path}: can't seal a login for that registry."),
        error,
    )
}

/// The host of `named`, the image Cloud says `service` runs, and the question that asks
/// to seal for it. Cloud's text is shown only as the host a whole image reference
/// yields, so it can't hide where the token goes, such as behind terminal escapes.
fn named_host(service: &ServiceName, named: Option<&str>) -> Result<(String, String), Error> {
    let path = format!("{service}.registryCredential");
    let image = named.ok_or_else(|| {
        Error::usage(format!(
            "{path}: only an image service takes registry credentials."
        ))
    })?;
    if !is_image_reference(image) {
        return Err(Error::usage(format!(
            "{path}: Ployz Cloud named an image this CLI can't read, so nothing was sent. \
             Name the registry, like --registry ghcr.io."
        )));
    }
    let host = login_host(image).map_err(|error| unreadable_host(&path, error))?;
    let question = format!("Seal {service}'s registry credentials for {host}?");
    Ok((host.to_owned(), question))
}

fn seal(
    recipient: &SealingPublicKey,
    identity: &SealedIdentity,
    key: &str,
    plaintext: &str,
) -> Result<ployz_core::config::ClientSealedValue, Error> {
    seal_value(recipient, identity, key, plaintext).map_err(|error| {
        Error::caused(
            RpcErrorCode::Internal,
            format!("{key}: could not seal the value"),
            error,
        )
    })
}

/// The Environment's ID as a sealed value binds to it.
fn bound(environment: &EnvironmentSummary) -> Result<Uuid, Error> {
    Uuid::parse_str(environment.id.as_str()).map_err(|_| {
        Error::conflict(format!(
            "{}/{} has no ID a sealed value binds to",
            environment.project, environment.name
        ))
    })
}

fn unexpected(what: &str) -> Error {
    Error::coded(
        RpcErrorCode::Internal,
        format!("Cloud answered with something other than {what}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cloud names the image, so the question shows only the host a whole reference
    /// yields: a reply holding terminal escapes is refused, never printed.
    #[test]
    fn the_question_never_shows_text_cloud_chose() {
        let web = ServiceName::parse("web").unwrap();
        let hidden = "ghcr.io/acme/web:1\x1b[2K\revil.example/acme/web:1";
        assert!(named_host(&web, Some(hidden)).is_err());
        let (host, question) = named_host(&web, Some("ghcr.io/acme/web:1")).unwrap();
        assert_eq!(host, "ghcr.io");
        assert!(!question.contains("acme"), "{question}");
        assert!(question.chars().all(|c| !c.is_control()), "{question}");
    }
}
