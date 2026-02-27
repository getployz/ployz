package workload

type DeployErrorPhase string

const (
	DeployErrorPhaseUnknown DeployErrorPhase = "unknown"
)

type DeployErrorReason string

const (
	DeployErrorReasonUnimplemented DeployErrorReason = "unimplemented"
)
