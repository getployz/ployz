//! A deploy signed with no person to ask: under a CI or agent key in `PLOYZ_KEY`, or
//! with `--yes`. Cloud relays the plan, so a key that signs it unread would sign whatever
//! Cloud planted. Instead the signer edits the last version a person approved: it reads
//! that version and the spec Cloud kept for it, checks the spec against the signed
//! digest, changes only the image `--image` names, and signs only when Cloud's plan comes
//! to the same digest. Each Server then admits the edit only beside a version a person
//! signed with the digest it names as its parent.

use std::collections::BTreeMap;

use clap::ArgMatches;
use ployz_core::authority::{
    ApprovedChange, Mounts, PayloadDigest, ServiceVersion, SignerKey, Timestamp,
    deploy::content_addressed,
};
use ployz_core::{
    Hint, Namespace, PARENT_VERSIONS_CAPABILITY, RequestedServiceSpec, RpcErrorCode,
    SCOPED_KEYS_CAPABILITY, SealingPublicKey, ServiceName,
};
use ployz_store::{EnvironmentRef, Parent, ParentQuery, PlanView, PostedVersion, TagPin};
use serde_json::{Value, json};

use super::super::device::DeviceSigning;
use super::super::store::Store;
use super::super::{Error, route};
use super::approve::DeviceApproval;
use crate::image::pin_public;

/// One `--image SERVICE=IMAGE`: the one change a silent deploy makes, and the
/// content-addressed image IMAGE is pinned to.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::handlers) struct ImageEdit {
    pub(in crate::handlers) service: ServiceName,
    /// As typed, which the Saved State keeps.
    pub(in crate::handlers) image: String,
    pub(in crate::handlers) pinned: String,
}

/// Each `--image SERVICE=IMAGE` typed, unpinned.
///
/// # Errors
/// Returns a usage error for one that isn't `SERVICE=IMAGE`, or names a Service twice.
pub(in crate::handlers) fn typed(
    matches: &ArgMatches,
) -> Result<Vec<(ServiceName, String)>, Error> {
    let mut typed: Vec<(ServiceName, String)> = Vec::new();
    for edit in matches.get_many::<String>("image").into_iter().flatten() {
        let Some((service, image)) = edit.split_once('=').filter(|(_, image)| !image.is_empty())
        else {
            return Err(Error::usage(
                "--image takes SERVICE=IMAGE, for example --image web=ghcr.io/acme/web:1.4",
            ));
        };
        let service = ServiceName::parse(service)?;
        if typed.iter().any(|(named, _)| *named == service) {
            return Err(Error::usage(format!("--image names {service} twice")));
        }
        typed.push((service, image.to_owned()));
    }
    Ok(typed)
}

/// `typed`, each image pinned by its registry, all at once. An image written by digest
/// needs no lookup.
///
/// # Errors
/// Returns why an image can't be pinned, naming the `--image` that asked for it.
pub(in crate::handlers) fn pinned(
    typed: Vec<(ServiceName, String)>,
) -> Result<Vec<ImageEdit>, Error> {
    let lookups = typed.iter().map(|(_, image)| async move {
        match content_addressed(image) {
            true => Ok(image.clone()),
            false => pin_public(image).await.map(|pin| pin.as_str().to_owned()),
        }
    });
    let found = super::super::runtime()?.block_on(futures_util::future::join_all(lookups));
    typed
        .into_iter()
        .zip(found)
        .map(|((service, image), found)| match found {
            Ok(pinned) => Ok(ImageEdit {
                service,
                image,
                pinned,
            }),
            Err(error) => Err(Error::detailed_caused(
                RpcErrorCode::Unavailable,
                format!("Couldn't pin the image --image gives {service}."),
                json!({ "service": service, "image": image }),
                Some(error),
            )),
        })
        .collect()
}

/// Whether this deploy signs with no person to ask: a key in `PLOYZ_KEY`, or `--yes`,
/// which `PLOYZ_AUTO_CONFIRM` also answers, at a terminal or not.
pub(in crate::handlers) fn silent(matches: &ArgMatches) -> bool {
    std::env::var_os(crate::env::KEY).is_some() || super::approve::flag(matches, "yes")
}

/// The CI or agent key `PLOYZ_KEY` holds, or `None` when it is unset.
///
/// # Errors
/// Returns a usage error when it holds something other than a key's 64 hex digits. The
/// value is never echoed.
pub(in crate::handlers) fn key() -> Result<Option<DeviceSigning>, Error> {
    let Some(value) = std::env::var_os(crate::env::KEY) else {
        return Ok(None);
    };
    let seed = value
        .to_str()
        .and_then(|text| hex::decode(text.trim()).ok())
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .ok_or_else(|| {
            Error::usage(format!(
                "{} must be the key's 64 hex digits, as ployz printed them when the key was made",
                crate::env::KEY
            ))
        })?;
    Ok(Some(DeviceSigning::from_seed(&seed)))
}

/// Refuse `--forget-older` before a silent deploy reads anything: only a person forgets.
///
/// # Errors
/// Returns `confirmation_required` when `--forget-older` was given.
pub(in crate::handlers) fn refuse_forget(matches: &ArgMatches) -> Result<(), Error> {
    if !super::approve::flag(matches, "forget-older") {
        return Ok(());
    }
    Err(Error::coded(
        RpcErrorCode::ConfirmationRequired,
        "--forget-older needs a terminal: --yes and CI or agent keys never forget. Nothing \
         signed or deployed.",
    ))
}

/// Check every Server can admit a CI or agent key's edit, as Cloud's list of them says.
/// Cloud can only make this refuse: a Server it says can, and can't, refuses the edit
/// itself.
///
/// # Errors
/// Refuses naming the Servers that lack a capability, or that Cloud saw without any;
/// returns Cloud's error when it can't list them.
pub(in crate::handlers) fn servers_ready(matches: &ArgMatches) -> Result<(), Error> {
    let runtime = super::super::runtime()?;
    let Some(credential) = route::cloud_runs(&runtime, matches)? else {
        return Err(Error::coded(
            RpcErrorCode::InvalidArgument,
            "A CI or agent key deploys through Ployz Cloud. Set PLOYZ_TOKEN or sign in. \
             Nothing signed or deployed.",
        ));
    };
    let answer = super::super::cloud_read::servers(&credential)?;
    let mut outdated = Vec::new();
    let mut unverified = Vec::new();
    for server in &answer.servers {
        let name = server.name.to_string();
        match &server.capabilities {
            None => unverified.push(name),
            Some(held)
                if !held.contains(SCOPED_KEYS_CAPABILITY)
                    || !held.contains(PARENT_VERSIONS_CAPABILITY) =>
            {
                outdated.push(name);
            }
            Some(_) => {}
        }
    }
    let capability = PARENT_VERSIONS_CAPABILITY;
    if !outdated.is_empty() {
        return Err(super::approve::outdated(matches, &outdated, capability));
    }
    if !unverified.is_empty() {
        return Err(super::approve::unverified(matches, &unverified, capability));
    }
    Ok(())
}

/// Sign `plan`'s Services as edits of the last versions a person approved, with
/// `signing`, changing only `edits`.
///
/// # Errors
/// Refuses before signing anything, as [`children`] says, and returns the Store's error
/// when it can't read the parents.
pub(in crate::handlers) fn sign(
    matches: &ArgMatches,
    store: &Store,
    (environment, plan): (&EnvironmentRef, PlanView),
    edits: &[ImageEdit],
    signing: &DeviceSigning,
) -> Result<DeviceApproval, Error> {
    let target = super::approve::signable(plan.target)?;
    let parents = store.read(&ParentQuery {
        environment: environment.clone(),
        services: target.iter().map(|spec| spec.name.clone()).collect(),
    })?;
    let children = children((&plan.namespace, &target), &parents.services, edits)
        .map_err(|refused| refused.error(matches))?;
    let shown: Vec<RequestedServiceSpec> =
        children.iter().map(|child| child.planned.clone()).collect();
    let pins: BTreeMap<ServiceName, String> = children
        .iter()
        .filter_map(|child| Some((child.service.clone(), child.pin.as_ref()?.image.clone())))
        .collect();
    let now = Timestamp::now();
    let mounts = Mounts {
        namespace: &plan.namespace,
        volumes: &plan.volumes,
        minted: &BTreeMap::new(),
        now,
    };
    let approved =
        ApprovedChange::of(&shown, &plan.namespace, &pins, &mounts).map_err(Error::command)?;
    for line in approved.iter().flat_map(ApprovedChange::lines) {
        crate::ui::note(format_args!("{line}"));
    }
    let versions = posted(children, signing, now)?;
    Ok(DeviceApproval {
        versions,
        version: plan.version,
        approved,
    })
}

/// `children` signed by `signing` at `now`, each naming its parent, with the pin it runs.
///
/// # Errors
/// Returns a command error when a body has no canonical form.
fn posted(
    children: Vec<Child>,
    signing: &DeviceSigning,
    now: Timestamp,
) -> Result<Vec<PostedVersion>, Error> {
    let signer = SignerKey::Device(signing.device_key());
    children
        .into_iter()
        .map(|child| {
            let statement = signing.sign(&ServiceVersion {
                cluster: child.cluster,
                namespace: child.namespace,
                service: child.service,
                digest: child.digest,
                signed_at: now,
                signer,
                proposal: None,
                forget_older: false,
                parent: Some(child.parent),
            })?;
            Ok(PostedVersion {
                statement,
                pin: child.pin,
                births: BTreeMap::new(),
            })
        })
        .collect()
}

/// One Service's edit of the last version a person approved, ready to sign.
#[derive(Debug, PartialEq)]
pub(super) struct Child {
    pub(super) service: ServiceName,
    /// The parent's, so the edit names the Cluster and Namespace a person approved.
    pub(super) cluster: SealingPublicKey,
    pub(super) namespace: Namespace,
    pub(super) digest: PayloadDigest,
    pub(super) parent: PayloadDigest,
    /// Cloud's plan of it, its image as the Saved State writes it.
    pub(super) planned: RequestedServiceSpec,
    pub(super) pin: Option<TagPin>,
}

/// Why a silent deploy signs nothing.
#[derive(Debug, PartialEq)]
pub(super) enum Refused {
    /// `--image` names a Service the deploy doesn't ship.
    NotShipped(ServiceName),
    /// No Deployment ran a version of it a person approved; `deployed` when it runs one
    /// nobody signed.
    NoParent {
        service: ServiceName,
        deployed: bool,
    },
    /// The spec Cloud keeps for its parent doesn't come to the digest the parent signed.
    Forged(ServiceName),
    /// Cloud's plan changes its Volumes or mounts.
    Mounts(ServiceName),
    /// Cloud's plan of it differs from the parent with the edit at `field`.
    Differs {
        service: ServiceName,
        field: String,
        /// The image Cloud's plan writes for it.
        image: String,
    },
}

/// Each Service of `target` in `namespace` as an edit of its parent in `parents`, which
/// changes only the image `edits` names for it, when Cloud's plan of it comes to the same
/// digest. A Service whose plan names its parent's tag runs its parent's pin.
///
/// # Errors
/// Returns the first [`Refused`], before anything is signed.
pub(super) fn children(
    (namespace, target): (&Namespace, &[RequestedServiceSpec]),
    parents: &BTreeMap<ServiceName, Parent>,
    edits: &[ImageEdit],
) -> Result<Vec<Child>, Refused> {
    if let Some(edit) = edits
        .iter()
        .find(|edit| !target.iter().any(|spec| spec.name == edit.service))
    {
        return Err(Refused::NotShipped(edit.service.clone()));
    }
    target
        .iter()
        .map(|planned| {
            let service = planned.name.clone();
            let (statement, approved, parent_pin) = match parents.get(&service) {
                Some(Parent::Signed {
                    statement,
                    spec,
                    pin,
                }) => (statement, spec, pin),
                Some(Parent::Missing { deployed }) => {
                    return Err(Refused::NoParent {
                        service,
                        deployed: *deployed,
                    });
                }
                None => {
                    return Err(Refused::NoParent {
                        service,
                        deployed: false,
                    });
                }
            };
            let Ok(parent) = statement.open() else {
                return Err(Refused::Forged(service));
            };
            if parent.service != service
                || PayloadDigest::of(approved, &parent.namespace).ok() != Some(parent.digest)
            {
                return Err(Refused::Forged(service));
            }
            let edit = edits.iter().find(|edit| edit.service == service);
            let mut child = (**approved).clone();
            if let Some(edit) = edit {
                child.container.image.clone_from(&edit.pinned);
            }
            let mut runs = planned.clone();
            let pin = match edit {
                Some(edit) if planned.container.image == edit.image => {
                    runs.container.image.clone_from(&edit.pinned);
                    (!content_addressed(&edit.image)).then(|| TagPin {
                        tag: edit.image.clone(),
                        image: edit.pinned.clone(),
                    })
                }
                Some(_) => None,
                None => parent_pin
                    .as_ref()
                    .filter(|pin| pin.tag == planned.container.image)
                    .inspect(|pin| runs.container.image.clone_from(&pin.image))
                    .cloned(),
            };
            if child.mount_graph != runs.mount_graph {
                return Err(Refused::Mounts(service));
            }
            let differs = |field: String| Refused::Differs {
                service: service.clone(),
                field,
                image: planned.container.image.clone(),
            };
            if parent.namespace != *namespace {
                return Err(differs("namespace".to_owned()));
            }
            let digest = PayloadDigest::of(&child, &parent.namespace)
                .map_err(|_| Refused::Forged(service.clone()))?;
            if PayloadDigest::of(&runs, namespace).ok() != Some(digest) {
                let (child, runs) = (json(&child), json(&runs));
                return Err(differs(
                    first_difference(&child, &runs).unwrap_or_else(|| "spec".to_owned()),
                ));
            }
            Ok(Child {
                service,
                cluster: parent.cluster,
                namespace: parent.namespace,
                digest,
                parent: parent.digest,
                planned: planned.clone(),
                pin,
            })
        })
        .collect()
}

fn json(spec: &RequestedServiceSpec) -> Value {
    serde_json::to_value(spec).unwrap_or(Value::Null)
}

/// The dotted path of the first place `left` and `right` differ, keys in order.
fn first_difference(left: &Value, right: &Value) -> Option<String> {
    match (left, right) {
        (Value::Object(left), Value::Object(right)) => {
            let mut keys: Vec<&String> = left.keys().chain(right.keys()).collect();
            keys.sort_unstable();
            keys.dedup();
            keys.into_iter().find_map(|key| {
                let (left, right) = (
                    left.get(key).unwrap_or(&Value::Null),
                    right.get(key).unwrap_or(&Value::Null),
                );
                first_difference(left, right).map(|below| match below.is_empty() {
                    true => key.clone(),
                    false => format!("{key}.{below}"),
                })
            })
        }
        _ if left == right => None,
        _ => Some(String::new()),
    }
}

impl Refused {
    fn error(self, matches: &ArgMatches) -> Error {
        match self {
            Self::NotShipped(service) => Error::detailed(
                RpcErrorCode::InvalidArgument,
                format!(
                    "--image names {service}, which this deploy doesn't ship. Name it in the \
                     deploy too. Nothing signed or deployed."
                ),
                json!({ "service": service }),
            ),
            Self::NoParent {
                service,
                deployed: false,
            } => Error::detailed(
                RpcErrorCode::ConfirmationRequired,
                format!(
                    "{service} has no approved version yet. Deploy it once from a terminal. \
                     Nothing signed or deployed."
                ),
                json!({ "service": service }),
            ),
            Self::NoParent {
                service,
                deployed: true,
            } => Error::detailed(
                RpcErrorCode::ConfirmationRequired,
                format!(
                    "{service} runs a version no person approved, such as one built from \
                     source. Switch it to an image and deploy it once from a terminal. Nothing \
                     signed or deployed."
                ),
                json!({ "service": service }),
            )
            .hint(Hint::Next(super::super::store::next(
                matches,
                &["set", &format!("{service}.image=IMAGE")],
            ))),
            Self::Forged(service) => Error::detailed(
                RpcErrorCode::Conflict,
                format!(
                    "Ployz Cloud's copy of {service}'s last approved version doesn't match its \
                     signature. Nothing signed or deployed."
                ),
                json!({ "service": service }),
            ),
            Self::Mounts(service) => Error::detailed(
                RpcErrorCode::ConfirmationRequired,
                format!(
                    "This deploy changes {service}'s volumes or mounts. Deploy it from a \
                     terminal. Nothing signed or deployed."
                ),
                json!({ "service": service }),
            ),
            Self::Differs {
                service,
                field,
                image,
            } if field.starts_with("container.image") => Error::detailed(
                RpcErrorCode::ConfirmationRequired,
                format!(
                    "{service}'s saved image isn't in its last approved version. Name it with \
                     --image. Nothing signed or deployed."
                ),
                json!({ "service": service, "field": field }),
            )
            .hint(Hint::Retry(super::super::typed_with(&[
                "--image",
                &format!("{service}={image}"),
            ]))),
            Self::Differs { service, field, .. } => Error::detailed(
                RpcErrorCode::ConfirmationRequired,
                format!(
                    "{service} changes more than its image, at {field}. Deploy it from a \
                     terminal. Nothing signed or deployed."
                ),
                json!({ "service": service, "field": field }),
            ),
        }
    }
}

#[cfg(test)]
#[path = "silent_tests.rs"]
mod tests;
