//! Acting in Cloud from the CLI: Organizations, Organization Tokens and signed-in
//! devices, through Cloud's `/api/cli` surface; and the Config Store's `read` and
//! `write`, through `/api/config`.
//!
//! Every call carries one credential: `PLOYZ_TOKEN` when set, else this device's
//! approved sign-in. Either acts in exactly one Organization.
//!
//! One request pipeline serves every call: `http` builds and sends the request and
//! reads the reply whole, `reply` turns a status and body into the caller's answer or
//! the error that names the fix, and the rest are the calls themselves.

mod calls;
mod credential;
mod http;
mod org;
mod reply;
mod servers;
mod store;
mod token;

#[cfg(test)]
mod testing;
#[cfg(test)]
mod transcript_tests;

pub(crate) use calls::{
    Settled, approved_answer, call, follow_operation, follow_run, keyed_reply, refusable,
    refusable_operation, start_run,
};
pub(crate) use credential::{
    Credential, TokenFrom, acting_as, acting_in, credential, env_token, from_env, named_cloud,
};
pub(crate) use http::{CALL_DEADLINE, Via, builder, client, notice_revision, revision_seen};
pub(crate) use org::{
    OrganizationEntry, Target, new_organization, organizations, remove_organization,
    use_organization,
};
pub(crate) use reply::{SIGN_IN_PAST_TOKEN, StoreCallError, public_error, refusal};
#[cfg(test)]
pub(crate) use servers::Reach;
pub(crate) use servers::{
    ForgetCheck, ObservedServer, Release, ServerAccess, ServersForgotten, check_forget_servers,
    follow_removal, forget_servers, server_access, start_forget, start_removal,
};
pub(crate) use store::{config_store, config_store_if_routed, upload};
pub(crate) use token::{
    CredentialKind, PresetKind, Removed, ServerClears, TokenKind, TokenPreset, credential_id,
    credentials, new_token, remove_token,
};
