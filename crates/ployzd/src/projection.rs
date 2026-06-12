#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionState<Projection, Error> {
    Current(Projection),
    LastKnownGood(Projection),
    ProjectionFailedRetained { retained: Projection, error: Error },
    ProjectionFailedUnavailable { error: Error },
    Unavailable,
}

impl<Projection, Error> ProjectionState<Projection, Error> {
    #[must_use]
    pub fn source_unavailable(self) -> Self {
        match self {
            Self::Current(projection) | Self::LastKnownGood(projection) => {
                Self::LastKnownGood(projection)
            }
            Self::ProjectionFailedRetained {
                retained: projection,
                error,
            } => Self::ProjectionFailedRetained {
                retained: projection,
                error,
            },
            Self::ProjectionFailedUnavailable { error } => {
                Self::ProjectionFailedUnavailable { error }
            }
            Self::Unavailable => Self::Unavailable,
        }
    }

    #[must_use]
    pub fn source_unavailable_with_error(self, error: Error) -> Self {
        match self {
            Self::Current(projection) | Self::LastKnownGood(projection) => {
                Self::ProjectionFailedRetained {
                    retained: projection,
                    error,
                }
            }
            Self::ProjectionFailedRetained {
                retained: projection,
                error: existing_error,
            } => Self::ProjectionFailedRetained {
                retained: projection,
                error: existing_error,
            },
            Self::ProjectionFailedUnavailable {
                error: existing_error,
            } => Self::ProjectionFailedUnavailable {
                error: existing_error,
            },
            Self::Unavailable => Self::ProjectionFailedUnavailable { error },
        }
    }

    #[must_use]
    pub fn source_failed(self, error: Error) -> Self {
        match self {
            Self::Current(projection)
            | Self::LastKnownGood(projection)
            | Self::ProjectionFailedRetained {
                retained: projection,
                ..
            } => Self::ProjectionFailedRetained {
                retained: projection,
                error,
            },
            Self::ProjectionFailedUnavailable { .. } | Self::Unavailable => {
                Self::ProjectionFailedUnavailable { error }
            }
        }
    }
}
