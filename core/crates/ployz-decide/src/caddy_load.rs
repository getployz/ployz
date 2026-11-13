//! What the Caddy watcher last left the Ingress Proxy serving, and what it may claim about
//! it.
//!
//! `watch_caddy` in `ployzd` runs the I/O of a pass and builds a [`PassOutcome`] from what
//! each call returned. Everything after that is here and pure: whether the next pass loads
//! anything ([`CaddyLoad::skip`]), what the pass left Caddy serving ([`CaddyLoad::after`])
//! and the routed addresses `ployz status` may report ([`CaddyLoad::claim`]). The verify
//! model for the watch loop calls these same functions.
//!
//! `I` is one pass's input and `A` the addresses a load routes; `ployzd` passes its
//! `CaddyInput` and routed addresses, the model passes small stand-ins.

/// What `ployzd` tells this module about an input to compare and to attribute.
pub trait Input {
    /// What names the Caddy that served the input.
    type Caddy: Eq;

    /// Whether `other` serves what this does.
    fn serves_as(&self, other: &Self) -> bool;

    /// The newest local ingress container the input was applied to, `None` without one.
    fn caddy(&self) -> Option<&Self::Caddy>;
}

/// Whether the Caddyfile a restarted Caddy reads holds what its admin loaded.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CaddyFile {
    Current,
    /// `write_caddyfile` failed, so the file may hold an earlier config.
    Stale,
}

/// What the watcher last left Caddy serving, carried from one pass to the next.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CaddyLoad<I, A> {
    /// Nothing the watcher applied is known to be served.
    Unapplied,
    /// The Caddyfile was written without the admin from `input`, or from an earlier input
    /// that serves as it and differs only in the trace page's values. A starting Caddy may
    /// have read the file before the write, so this confirms nothing.
    Written { input: I },
    /// The admin of `input.caddy` loaded `input`, so Caddy serves `addresses`, and the files
    /// a restart reads hold it.
    Loaded { input: I, addresses: A },
    /// The admin of `input.caddy` loaded `input`, then `apply_caddy` failed, so the next pass
    /// applies it again.
    Served {
        input: I,
        addresses: A,
        file: CaddyFile,
    },
}

/// How a pass left Caddy, as [`CaddyLoad::after`] reads it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PassOutcome<I, A> {
    /// [`CaddyLoad::skip`] held through the admin, so nothing was loaded.
    Cached { addresses: A },
    /// Without the admin, the Caddyfile holds `input`.
    Written { input: I },
    /// `connect` failed, or a write without the admin did.
    Unknown,
    /// `apply_caddy` through the admin, and `find_refused` after a refusal.
    Loaded {
        input: I,
        addresses: A,
        applied: Applied,
    },
}

impl<I, A> PassOutcome<I, A> {
    #[must_use]
    pub fn through_admin(&self) -> bool {
        matches!(self, Self::Cached { .. } | Self::Loaded { .. })
    }

    /// The probe found Caddy refusing a certificate, so the pass derives its input again
    /// without it.
    #[must_use]
    pub fn found_refusal(&self) -> bool {
        matches!(
            self,
            Self::Loaded {
                applied: Applied::BeforeServing(Probe::Refused),
                ..
            }
        )
    }
}

/// What `apply_caddy` through the admin left Caddy serving.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Applied {
    Ok,
    AfterServing(CaddyFile),
    BeforeServing(Probe),
}

/// What a probe left Caddy serving.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Probe {
    /// Caddy still serves the config it had.
    Unchanged,
    /// Caddy serves a probe config, though it refused no certificate.
    Reloaded,
    /// Caddy refused at least one certificate, now quarantined.
    Refused,
}

impl<I: Input, A> CaddyLoad<I, A> {
    /// Caddy already serves `input`, so the pass loads nothing. Through the admin, its last
    /// load is `input` and the files hold it; without, the Caddyfile was written from it.
    #[must_use]
    pub fn skip(&self, input: &I, through_admin: bool) -> bool {
        match self {
            Self::Loaded { input: loaded, .. } => through_admin && loaded.serves_as(input),
            Self::Written { input: written } => !through_admin && written.serves_as(input),
            Self::Unapplied | Self::Served { .. } => false,
        }
    }

    #[must_use]
    pub fn after(self, pass: PassOutcome<I, A>) -> Self {
        match pass {
            PassOutcome::Cached { addresses } => match self {
                Self::Loaded { input, .. } => Self::Loaded { input, addresses },
                // `skip` holds through the admin only on a load.
                Self::Unapplied | Self::Written { .. } | Self::Served { .. } => self,
            },
            PassOutcome::Written { input } => Self::Written { input },
            PassOutcome::Unknown => Self::Unapplied,
            PassOutcome::Loaded {
                input,
                addresses,
                applied,
            } => match applied {
                Applied::Ok => Self::Loaded { input, addresses },
                Applied::AfterServing(file) => Self::Served {
                    input,
                    addresses,
                    file,
                },
                Applied::BeforeServing(Probe::Unchanged) => self.refused_by(input.caddy()),
                Applied::BeforeServing(Probe::Reloaded | Probe::Refused) => Self::Unapplied,
            },
        }
    }

    /// A refused load leaves Caddy serving its last load, and a restart under the same id
    /// serves the Caddyfile, so the load stands only for the newest ingress container and
    /// while the file holds it. A new Caddy serves its Caddyfile, which may predate this
    /// watcher. A restart between the load and the Caddyfile write goes unseen (#2787).
    #[must_use]
    pub fn refused_by(self, caddy: Option<&I::Caddy>) -> Self {
        let stands = match &self {
            Self::Loaded { input, .. }
            | Self::Served {
                input,
                file: CaddyFile::Current,
                ..
            } => input.caddy().is_some() && input.caddy() == caddy,
            Self::Written { .. } => true,
            Self::Unapplied
            | Self::Served {
                file: CaddyFile::Stale,
                ..
            } => false,
        };
        if stands { self } else { Self::Unapplied }
    }
}

impl<I, A: Clone + Default> CaddyLoad<I, A> {
    /// `PublishedRoutes.addresses` after a pass. Without the admin, a running Caddy serves
    /// what it last loaded or, after a restart, the Caddyfile, so only a stopped Caddy's
    /// empty routes are claimed.
    #[must_use]
    pub fn claim(&self, through_admin: bool, no_caddy: bool) -> Option<A> {
        if !through_admin {
            return no_caddy.then(A::default);
        }
        match self {
            Self::Loaded { addresses, .. } | Self::Served { addresses, .. } => {
                Some(addresses.clone())
            }
            Self::Unapplied | Self::Written { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An input is its trace page's value and the container; `serves_as` leaves the value out.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct Pass {
        page: u8,
        caddy: Option<u8>,
    }

    impl Input for Pass {
        type Caddy = u8;

        fn serves_as(&self, other: &Self) -> bool {
            self.caddy == other.caddy
        }

        fn caddy(&self) -> Option<&u8> {
            self.caddy.as_ref()
        }
    }

    type Load = CaddyLoad<Pass, u8>;

    const X: Pass = Pass {
        page: 0,
        caddy: Some(1),
    };
    const X_NEW_PAGE: Pass = Pass {
        page: 1,
        caddy: Some(1),
    };
    const Y: Pass = Pass {
        page: 0,
        caddy: Some(2),
    };
    const NO_CADDY: Pass = Pass {
        page: 0,
        caddy: None,
    };

    fn loaded(input: Pass) -> Load {
        CaddyLoad::Loaded {
            input,
            addresses: 7,
        }
    }

    fn served(input: Pass, file: CaddyFile) -> Load {
        CaddyLoad::Served {
            input,
            addresses: 7,
            file,
        }
    }

    fn pass(input: Pass, applied: Applied) -> PassOutcome<Pass, u8> {
        PassOutcome::Loaded {
            input,
            addresses: 9,
            applied,
        }
    }

    #[test]
    fn skip_holds_only_for_the_route_the_last_load_took() {
        let table: [(Load, bool, bool); 8] = [
            (loaded(X), true, true),
            (loaded(X), false, false),
            (CaddyLoad::Written { input: X }, false, true),
            (CaddyLoad::Written { input: X }, true, false),
            (CaddyLoad::Unapplied, true, false),
            (CaddyLoad::Unapplied, false, false),
            (served(X, CaddyFile::Current), true, false),
            (served(X, CaddyFile::Stale), false, false),
        ];
        for (load, through_admin, skips) in table {
            assert_eq!(
                load.skip(&X, through_admin),
                skips,
                "{load:?} {through_admin}"
            );
        }
        assert!(
            loaded(X).skip(&X_NEW_PAGE, true),
            "the page's value is not compared"
        );
        assert!(!loaded(X).skip(&Y, true));
    }

    #[test]
    fn a_pass_leaves_the_load_its_outcome_names() {
        let table: [(Load, PassOutcome<Pass, u8>, Load); 14] = [
            (
                loaded(X),
                PassOutcome::Cached { addresses: 9 },
                CaddyLoad::Loaded {
                    input: X,
                    addresses: 9,
                },
            ),
            (
                CaddyLoad::Unapplied,
                PassOutcome::Cached { addresses: 9 },
                CaddyLoad::Unapplied,
            ),
            (
                served(X, CaddyFile::Stale),
                PassOutcome::Cached { addresses: 9 },
                served(X, CaddyFile::Stale),
            ),
            (
                loaded(X),
                PassOutcome::Written { input: Y },
                CaddyLoad::Written { input: Y },
            ),
            (loaded(X), PassOutcome::Unknown, CaddyLoad::Unapplied),
            (
                CaddyLoad::Unapplied,
                pass(Y, Applied::Ok),
                CaddyLoad::Loaded {
                    input: Y,
                    addresses: 9,
                },
            ),
            (
                CaddyLoad::Unapplied,
                pass(Y, Applied::AfterServing(CaddyFile::Stale)),
                CaddyLoad::Served {
                    input: Y,
                    addresses: 9,
                    file: CaddyFile::Stale,
                },
            ),
            (
                CaddyLoad::Unapplied,
                pass(Y, Applied::AfterServing(CaddyFile::Current)),
                CaddyLoad::Served {
                    input: Y,
                    addresses: 9,
                    file: CaddyFile::Current,
                },
            ),
            (
                loaded(X),
                pass(X_NEW_PAGE, Applied::BeforeServing(Probe::Unchanged)),
                loaded(X),
            ),
            (
                loaded(X),
                pass(Y, Applied::BeforeServing(Probe::Unchanged)),
                CaddyLoad::Unapplied,
            ),
            (
                served(X, CaddyFile::Current),
                pass(X, Applied::BeforeServing(Probe::Unchanged)),
                served(X, CaddyFile::Current),
            ),
            (
                served(X, CaddyFile::Stale),
                pass(X, Applied::BeforeServing(Probe::Unchanged)),
                CaddyLoad::Unapplied,
            ),
            (
                loaded(X),
                pass(X, Applied::BeforeServing(Probe::Reloaded)),
                CaddyLoad::Unapplied,
            ),
            (
                loaded(X),
                pass(X, Applied::BeforeServing(Probe::Refused)),
                CaddyLoad::Unapplied,
            ),
        ];
        for (before, outcome, after) in table {
            assert_eq!(before.after(outcome), after, "{before:?} then {outcome:?}");
        }
    }

    #[test]
    fn a_refusal_stands_for_the_newest_container_only() {
        let written: Load = CaddyLoad::Written { input: X };
        assert_eq!(
            written.refused_by(Some(&2)),
            written,
            "a Caddyfile written without the admin stays"
        );
        assert_eq!(loaded(X).refused_by(Some(&1)), loaded(X));
        assert_eq!(loaded(X).refused_by(Some(&2)), CaddyLoad::Unapplied);
        assert_eq!(loaded(X).refused_by(None), CaddyLoad::Unapplied);
        assert_eq!(loaded(NO_CADDY).refused_by(None), CaddyLoad::Unapplied);
    }

    #[test]
    fn claim_names_what_the_admin_confirmed() {
        let table: [(Load, bool, bool, Option<u8>); 9] = [
            (loaded(X), true, false, Some(7)),
            (served(X, CaddyFile::Stale), true, false, Some(7)),
            (CaddyLoad::Written { input: X }, true, false, None),
            (CaddyLoad::Unapplied, true, true, None),
            (loaded(X), false, false, None),
            (loaded(X), false, true, Some(0)),
            (CaddyLoad::Written { input: X }, false, true, Some(0)),
            (CaddyLoad::Unapplied, false, true, Some(0)),
            (CaddyLoad::Unapplied, false, false, None),
        ];
        for (load, through_admin, no_caddy, claim) in table {
            assert_eq!(
                load.claim(through_admin, no_caddy),
                claim,
                "{load:?} {through_admin} {no_caddy}"
            );
        }
    }

    #[test]
    fn only_a_probe_refusal_derives_again() {
        for (applied, derives) in [
            (Applied::Ok, false),
            (Applied::AfterServing(CaddyFile::Stale), false),
            (Applied::BeforeServing(Probe::Unchanged), false),
            (Applied::BeforeServing(Probe::Reloaded), false),
            (Applied::BeforeServing(Probe::Refused), true),
        ] {
            assert_eq!(pass(X, applied).found_refusal(), derives, "{applied:?}");
        }
        assert!(!PassOutcome::<Pass, u8>::Unknown.found_refusal());
        assert!(PassOutcome::<Pass, u8>::Cached { addresses: 0 }.through_admin());
        assert!(!PassOutcome::<Pass, u8>::Written { input: X }.through_admin());
    }
}
