package deploy

type PlanReasonCode uint8

const (
	PlanReasonUnknown PlanReasonCode = iota + 1
	PlanReasonUpToDate
	PlanReasonSpecMetadataChanged
	PlanReasonNeedsUpdate
	PlanReasonNeedsRecreate
	PlanReasonCurrentSpecDecodeFailed
	PlanReasonCreateNewService
	PlanReasonCreateScaleUp
	PlanReasonCreateNewAssignment
	PlanReasonRemoveService
	PlanReasonRemoveScaleDown
	PlanReasonRemoveStaleAssignment
)

func (c PlanReasonCode) String() string {
	switch c {
	case PlanReasonUnknown:
		return "unknown"
	case PlanReasonUpToDate:
		return "up_to_date"
	case PlanReasonSpecMetadataChanged:
		return "spec_metadata_changed"
	case PlanReasonNeedsUpdate:
		return "needs_update"
	case PlanReasonNeedsRecreate:
		return "needs_recreate"
	case PlanReasonCurrentSpecDecodeFailed:
		return "current_spec_decode_failed"
	case PlanReasonCreateNewService:
		return "create_new_service"
	case PlanReasonCreateScaleUp:
		return "create_scale_up"
	case PlanReasonCreateNewAssignment:
		return "create_new_assignment"
	case PlanReasonRemoveService:
		return "remove_service"
	case PlanReasonRemoveScaleDown:
		return "remove_scale_down"
	case PlanReasonRemoveStaleAssignment:
		return "remove_stale_assignment"
	default:
		return "unknown"
	}
}

func (c PlanReasonCode) IsValid() bool {
	switch c {
	case PlanReasonUnknown,
		PlanReasonUpToDate,
		PlanReasonSpecMetadataChanged,
		PlanReasonNeedsUpdate,
		PlanReasonNeedsRecreate,
		PlanReasonCurrentSpecDecodeFailed,
		PlanReasonCreateNewService,
		PlanReasonCreateScaleUp,
		PlanReasonCreateNewAssignment,
		PlanReasonRemoveService,
		PlanReasonRemoveScaleDown,
		PlanReasonRemoveStaleAssignment:
		return true
	default:
		return false
	}
}

type DeployErrorReason uint8

const (
	DeployErrorReasonUnknown DeployErrorReason = iota + 1
	DeployErrorReasonContextCanceled
	DeployErrorReasonOwnershipCheckFailed
	DeployErrorReasonImagePullFailed
	DeployErrorReasonExecutionFailed
	DeployErrorReasonHealthCheckFailed
	DeployErrorReasonPostconditionReadFailed
	DeployErrorReasonPostconditionMismatch
)

func (r DeployErrorReason) String() string {
	switch r {
	case DeployErrorReasonUnknown:
		return "unknown"
	case DeployErrorReasonContextCanceled:
		return "context_canceled"
	case DeployErrorReasonOwnershipCheckFailed:
		return "ownership_check_failed"
	case DeployErrorReasonImagePullFailed:
		return "image_pull_failed"
	case DeployErrorReasonExecutionFailed:
		return "execution_failed"
	case DeployErrorReasonHealthCheckFailed:
		return "health_check_failed"
	case DeployErrorReasonPostconditionReadFailed:
		return "postcondition_read_failed"
	case DeployErrorReasonPostconditionMismatch:
		return "postcondition_mismatch"
	default:
		return "unknown"
	}
}

func (r DeployErrorReason) IsValid() bool {
	switch r {
	case DeployErrorReasonUnknown,
		DeployErrorReasonContextCanceled,
		DeployErrorReasonOwnershipCheckFailed,
		DeployErrorReasonImagePullFailed,
		DeployErrorReasonExecutionFailed,
		DeployErrorReasonHealthCheckFailed,
		DeployErrorReasonPostconditionReadFailed,
		DeployErrorReasonPostconditionMismatch:
		return true
	default:
		return false
	}
}

func defaultDeployErrorReasonForPhase(phase DeployErrorPhase) DeployErrorReason {
	switch phase {
	case DeployErrorPhaseOwnership:
		return DeployErrorReasonOwnershipCheckFailed
	case DeployErrorPhasePrePull:
		return DeployErrorReasonImagePullFailed
	case DeployErrorPhaseExecute:
		return DeployErrorReasonExecutionFailed
	case DeployErrorPhaseHealth:
		return DeployErrorReasonHealthCheckFailed
	case DeployErrorPhasePostcondition:
		return DeployErrorReasonPostconditionMismatch
	default:
		return DeployErrorReasonUnknown
	}
}
