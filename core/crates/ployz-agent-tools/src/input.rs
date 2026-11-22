//! The field groups most tool inputs share, written once. `serde(flatten)`
//! cannot carry them: it refuses `deny_unknown_fields`, which every tool input
//! relies on to turn a mistyped field into a refusal. So [`tool_input!`] expands
//! each group to the field text a tool would write by hand, in the position the
//! tool gives it, and the traits let [`Cx`](super::machine::Cx) read a group
//! without knowing the tool.

use ployz_store::{EnvironmentName, ProjectName};

/// A tool that names where it runs: a Project and an Environment, each
/// defaulting to the place the client is in.
pub(crate) trait HasPlace {
    fn project(&self) -> Option<&ProjectName>;
    fn environment(&self) -> Option<&EnvironmentName>;
}

/// Declare a tool's input struct. Ordinary fields are written as in a struct;
/// a group stands in for its fields at the same position:
///
/// - `@place`: `project` and `environment`
/// - `@revision`: `expect_revision`
/// - `@wait`: `wait_seconds`, capped at `WAIT_CAP_SECONDS`
/// - `@key`: `request_key`
/// - `@loss`: `accept_volume_loss` as qualified names and the `version` that goes
///   with them, for the tools that turn them into an `AcceptedLoss`
///
/// Every group documents its fields, so a model reads the same sentence wherever
/// one appears. `@place["..." "..."]`, `@wait["..."]` and `@key["..."]` give a tool
/// whose field means something else its own text.
macro_rules! tool_input {
    ($vis:vis struct $name:ident { $($body:tt)* }) => {
        tool_input!(@fields {$vis $name} [] [] $($body)*);
    };
    (@fields {$vis:vis $name:ident} [$($out:tt)*] [$($impls:tt)*]) => {
        #[derive(::serde::Deserialize, ::schemars::JsonSchema)]
        #[serde(deny_unknown_fields)]
        $vis struct $name { $($out)* }
        $($impls)*
    };
    (@fields $head:tt $out:tt $impls:tt @place(named), $($rest:tt)*) => {
        tool_input!(@fields $head $out $impls
            @place[
                "Project name. Defaults to the one you are in."
                "Environment name, e.g. `staging`. Defaults to the one you are in."
            ], $($rest)*);
    };
    (@fields $head:tt $out:tt $impls:tt @place(here), $($rest:tt)*) => {
        tool_input!(@fields $head $out $impls @place[
            "Project name. Defaults to the one you are in."
            "Environment name, e.g. `staging`. Defaults to the one you are in."
        ], $($rest)*);
    };
    (@fields $head:tt $out:tt $impls:tt @place, $($rest:tt)*) => {
        tool_input!(@fields $head $out $impls @place(here), $($rest)*);
    };
    (
        @fields {$vis:vis $name:ident} [$($out:tt)*] [$($impls:tt)*]
        @place [$project:literal $environment:literal], $($rest:tt)*
    ) => {
        tool_input!(@fields {$vis $name} [$($out)*
            #[doc = $project]
            #[serde(default)]
            #[schemars(with = "Option<String>", pattern(::ployz_store::catalog::NODE_NAME))]
            project: Option<::ployz_store::ProjectName>,
            #[doc = $environment]
            #[serde(default)]
            #[schemars(with = "Option<String>", pattern(::ployz_store::catalog::NODE_NAME))]
            environment: Option<::ployz_store::EnvironmentName>,
        ] [$($impls)*
            impl $crate::input::HasPlace for $name {
                fn project(&self) -> Option<&::ployz_store::ProjectName> {
                    self.project.as_ref()
                }
                fn environment(&self) -> Option<&::ployz_store::EnvironmentName> {
                    self.environment.as_ref()
                }
            }
        ] $($rest)*);
    };
    (@fields $head:tt [$($out:tt)*] $impls:tt @revision, $($rest:tt)*) => {
        tool_input!(@fields $head [$($out)*
            /// The revision from your last read. Fails if the Environment changed since.
            #[serde(default)]
            expect_revision: Option<u64>,
        ] $impls $($rest)*);
    };
    (@fields $head:tt [$($out:tt)*] $impls:tt @loss, $($rest:tt)*) => {
        tool_input!(@fields $head [$($out)*
            /// Volumes whose data the member agreed to lose, named as the refusal listed them.
            #[serde(default)]
            accept_volume_loss: Vec<String>,
            /// The version the refusal gave. Fails if what it covers changed since.
            #[serde(default)]
            version: Option<String>,
        ] $impls $($rest)*);
    };
    (@fields $head:tt $out:tt $impls:tt @wait, $($rest:tt)*) => {
        tool_input!(@fields $head $out $impls @wait[
            "Seconds to wait for the outcome. Default 0: returns as soon as it is accepted."
        ], $($rest)*);
    };
    (
        @fields $head:tt [$($out:tt)*] $impls:tt
        @wait [$doc:literal], $($rest:tt)*
    ) => {
        tool_input!(@fields $head [$($out)*
            #[doc = $doc]
            #[serde(default)]
            #[schemars(range(min = 0, max = $crate::WAIT_CAP_SECONDS))]
            wait_seconds: u32,
        ] $impls $($rest)*);
    };
    (@fields $head:tt $out:tt $impls:tt @key, $($rest:tt)*) => {
        tool_input!(@fields $head $out $impls @key[
            "Any key you pick for this run. Reuse it to get that run back instead of starting another."
        ], $($rest)*);
    };
    (
        @fields $head:tt [$($out:tt)*] $impls:tt
        @key [$doc:literal], $($rest:tt)*
    ) => {
        tool_input!(@fields $head [$($out)*
            #[doc = $doc]
            #[serde(default)]
            #[schemars(
                with = "Option<String>",
                pattern($crate::RequestKey::PATTERN)
            )]
            request_key: Option<$crate::RequestKey>,
        ] $impls $($rest)*);
    };
    (
        @fields $head:tt [$($out:tt)*] $impls:tt
        $(#[$meta:meta])* $field:ident: $ty:ty, $($rest:tt)*
    ) => {
        tool_input!(@fields $head [$($out)* $(#[$meta])* $field: $ty,] $impls $($rest)*);
    };
}

pub(crate) use tool_input;
