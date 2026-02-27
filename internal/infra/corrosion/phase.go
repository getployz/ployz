package corrosion

type HealthPhase uint8

const (
	HealthUnreachable HealthPhase = iota + 1
	HealthForming
	HealthSyncing
	HealthReady
)

type HealthSample struct {
	Reachable     bool
	ThresholdsMet bool
	Members       int
	Gaps          uint64
	QueueSize     uint64
}

func (p HealthPhase) String() string {
	switch p {
	case HealthUnreachable:
		return "unreachable"
	case HealthForming:
		return "forming"
	case HealthSyncing:
		return "syncing"
	case HealthReady:
		return "ready"
	default:
		return "unknown"
	}
}

func ClassifyHealth(sample HealthSample, expectedMembers int) HealthPhase {
	if expectedMembers < 1 {
		expectedMembers = 1
	}
	if !sample.Reachable {
		return HealthUnreachable
	}
	if sample.Members < expectedMembers {
		return HealthForming
	}
	if !sample.ThresholdsMet || sample.Gaps > 0 || sample.QueueSize > 0 {
		return HealthSyncing
	}
	return HealthReady
}
