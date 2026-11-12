//! What a Docker status code means to a caller, written once.

use bollard::errors::Error as DockerError;
use ployz_core::ContainerId;

use super::Error;

/// How a Docker call's failure bears on an idempotent caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    /// 404: the Container, image or volume is not there.
    Absent,
    /// 409: the name is taken, or the object is in use.
    Conflict,
    /// 304: the call asked for a state the object already holds.
    Unchanged,
    Other,
}

pub(crate) fn classify(error: &DockerError) -> Status {
    let DockerError::DockerResponseServerError { status_code, .. } = error else {
        return Status::Other;
    };
    match status_code {
        404 => Status::Absent,
        409 => Status::Conflict,
        304 => Status::Unchanged,
        _ => Status::Other,
    }
}

/// `error` as the module's [`Error`], an absent Container as `ContainerNotFound`.
pub(crate) fn docker_error(container_id: &ContainerId, error: DockerError) -> Error {
    match classify(&error) {
        Status::Absent => Error::ContainerNotFound(*container_id),
        Status::Conflict | Status::Unchanged | Status::Other => Error::Docker(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(status_code: u16) -> DockerError {
        DockerError::DockerResponseServerError {
            status_code,
            message: "refused".into(),
        }
    }

    #[test]
    fn each_status_code_a_caller_treats_specially_has_its_own_class() {
        assert_eq!(classify(&refused(404)), Status::Absent);
        assert_eq!(classify(&refused(409)), Status::Conflict);
        assert_eq!(classify(&refused(304)), Status::Unchanged);
        assert_eq!(classify(&refused(500)), Status::Other);
        assert_eq!(classify(&DockerError::RequestTimeoutError), Status::Other);
    }

    #[test]
    fn only_an_absent_container_becomes_container_not_found() {
        let id = ContainerId::parse("a".repeat(64)).unwrap();
        assert!(matches!(
            docker_error(&id, refused(404)),
            Error::ContainerNotFound(found) if found == id
        ));
        assert!(matches!(
            docker_error(&id, refused(409)),
            Error::Docker(DockerError::DockerResponseServerError {
                status_code: 409,
                ..
            })
        ));
    }
}
