//! Talking to Ployz Cloud: one HTTP client and its timeouts ([`http`]), one reader of
//! what Cloud answers ([`reply`]), the Cloud a command talks to ([`origin`]), signing in
//! ([`login`]), acting in an Organization ([`account`]), and enrolling a Server
//! ([`enroll`]).

pub(crate) mod account;
pub(crate) mod enroll;
pub(crate) mod http;
pub(crate) mod login;
pub(crate) mod origin;
pub(crate) mod reply;
