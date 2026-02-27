package workload

type DeployPhase string

const (
	DeployPhaseUnknown       DeployPhase = "unknown"
	DeployPhaseUnimplemented DeployPhase = "unimplemented"
)

type DeployError struct {
	Phase   DeployErrorPhase
	Reason  DeployErrorReason
	Message string
}

func (e *DeployError) Error() string {
	if e == nil {
		return "workload: not yet implemented"
	}
	if e.Message != "" {
		return e.Message
	}
	return "workload: not yet implemented"
}

type ProgressEvent struct {
	Type    string
	Message string
}
