//! Internal distributed-control support primitives for Ployz.
//!
//! Polis owns product-neutral mechanics such as identity, authority, records,
//! projections, operation evidence, claims, and bounded calls. It must not know
//! Ployz product domains such as deploys, certificates, routes, runtime
//! participants, or volumes.
//!
//! Production rules:
//! - primitives expose typed failures, not display-string contracts;
//! - durable evidence is not product truth until a Ployz verifier accepts it;
//! - claims are advisory until a product resource enforces the fence;
//! - external I/O must be deadline-bounded by the adapter using the primitive.

pub mod error;

pub use error::{Error, Result};

#[cfg(test)]
mod tests {
    #[test]
    fn crate_has_no_product_modules() {
        let public_modules = ["error"];

        assert!(!public_modules.contains(&"deploy"));
        assert!(!public_modules.contains(&"acme"));
        assert!(!public_modules.contains(&"serving"));
        assert!(!public_modules.contains(&"runtime"));
        assert!(!public_modules.contains(&"volume"));
    }
}
