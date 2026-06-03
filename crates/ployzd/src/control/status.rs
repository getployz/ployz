use ployz_api::ProtocolError;
use ployz_api::status::{
    CorrosionCleanupDto, CorrosionFailureDto, CorrosionPhaseDto, CorrosionStopDto, PeerFailureDto,
    PeerPhaseDto, SchemaFailureDto, SchemaPhaseDto,
};
use ployz_api::status::{OperationFailureSummaryDto, StartupReportDto};

use crate::commands::DaemonStatus;
use crate::report::{
    CorrosionCleanup, CorrosionFailure, CorrosionPhase, PeerFailure, PeerPhase, SchemaFailure,
    SchemaPhase, StartupReport,
};

pub(super) fn status_dto(value: &DaemonStatus) -> ployz_api::status::StatusResponseDto {
    let startup = value.startup_report();
    ployz_api::status::StatusResponseDto::new(
        value.command_service_ready(),
        startup.is_ready(),
        startup
            .endpoint_id()
            .map(|endpoint| endpoint.as_str().to_owned()),
        startup_report_dto(startup),
        value.last_operation_failure().map(|failure| {
            OperationFailureSummaryDto::new(
                failure.operation().as_str(),
                ProtocolError::from(failure.source().clone()).code(),
            )
        }),
    )
}

fn startup_report_dto(value: &StartupReport) -> StartupReportDto {
    StartupReportDto::new(
        corrosion_phase(value.corrosion().phase()),
        corrosion_cleanup(value.corrosion().cleanup()),
        schema_phase(value.schema().phase()),
        peer_phase(value.peer().phase()),
        value.corrosion_api_addr().to_string(),
        value.peer_identity_path().display().to_string(),
    )
}

fn corrosion_phase(value: CorrosionPhase) -> CorrosionPhaseDto {
    match value {
        CorrosionPhase::Configured => CorrosionPhaseDto::Configured,
        CorrosionPhase::Ready => CorrosionPhaseDto::Ready,
        CorrosionPhase::Failed(failure) => CorrosionPhaseDto::Failed {
            failure: corrosion_failure(failure),
        },
    }
}

fn corrosion_failure(value: CorrosionFailure) -> CorrosionFailureDto {
    match value {
        CorrosionFailure::Agent => CorrosionFailureDto::Agent,
        CorrosionFailure::Store => CorrosionFailureDto::Store,
    }
}

fn corrosion_cleanup(value: CorrosionCleanup) -> CorrosionCleanupDto {
    match value {
        CorrosionCleanup::NotRun => CorrosionCleanupDto::NotRun,
        CorrosionCleanup::Stopped(crate::report::CorrosionStop::Graceful) => {
            CorrosionCleanupDto::Stopped {
                stop: CorrosionStopDto::Graceful,
            }
        }
        CorrosionCleanup::Stopped(crate::report::CorrosionStop::Forced) => {
            CorrosionCleanupDto::Stopped {
                stop: CorrosionStopDto::Forced,
            }
        }
        CorrosionCleanup::Stopped(crate::report::CorrosionStop::UnexpectedExit) => {
            CorrosionCleanupDto::Stopped {
                stop: CorrosionStopDto::UnexpectedExit,
            }
        }
        CorrosionCleanup::Unmanaged => CorrosionCleanupDto::Unmanaged,
        CorrosionCleanup::Failed => CorrosionCleanupDto::Failed,
    }
}

fn schema_phase(value: SchemaPhase) -> SchemaPhaseDto {
    match value {
        SchemaPhase::Configured => SchemaPhaseDto::Configured,
        SchemaPhase::Ready => SchemaPhaseDto::Ready,
        SchemaPhase::Failed(failure) => SchemaPhaseDto::Failed {
            failure: schema_failure(failure),
        },
    }
}

fn schema_failure(value: SchemaFailure) -> SchemaFailureDto {
    match value {
        SchemaFailure::Verify => SchemaFailureDto::Verify,
    }
}

fn peer_phase(value: PeerPhase) -> PeerPhaseDto {
    match value {
        PeerPhase::Configured => PeerPhaseDto::Configured,
        PeerPhase::Ready => PeerPhaseDto::Ready,
        PeerPhase::Failed(failure) => PeerPhaseDto::Failed {
            failure: peer_failure(failure),
        },
        PeerPhase::Stopped => PeerPhaseDto::Stopped,
    }
}

fn peer_failure(value: PeerFailure) -> PeerFailureDto {
    match value {
        PeerFailure::Startup => PeerFailureDto::Startup,
        PeerFailure::Shutdown => PeerFailureDto::Shutdown,
    }
}
