//! Generated TypeScript declarations and Rust examples for the `@ployz/sdk` napi package.

mod generate;
mod values;

pub use generate::{PACKAGE_NAME, decode_fixture, drift, sdk_package_root, write_generated};
pub use values::fixtures;

/// Serialize a public SDK Watch frame with Services derived from its Containers.
/// The RPC frame retains only the canonical Container observations.
#[must_use]
pub fn runtime_watch_view(frame: &ployz_core::RuntimeWatchFrame) -> impl serde::Serialize + '_ {
    #[derive(serde::Serialize)]
    struct View<'frame> {
        #[serde(flatten)]
        frame: &'frame ployz_core::RuntimeWatchFrame,
        services: Vec<ployz_core::ServiceObservation>,
    }
    View {
        frame,
        services: frame.services(),
    }
}
