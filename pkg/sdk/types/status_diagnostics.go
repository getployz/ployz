package types

import "strings"

// StatusIssue is a structured diagnosis issue derived from runtime state.
type StatusIssue struct {
	Component string
	Phase     string
	Message   string
	Hint      string
}

// ServiceBlockerIssues returns hard blockers for running service workloads.
func (s NetworkStatus) ServiceBlockerIssues() []StatusIssue {
	issues := make([]StatusIssue, 0, 8)

	networkPhase := strings.TrimSpace(s.NetworkPhase)
	if !s.Configured || networkPhase == "unconfigured" || networkPhase == "" {
		issues = append(issues, StatusIssue{
			Component: "config",
			Phase:     phaseOrUnknown(networkPhase),
			Message:   "network is not configured",
			Hint:      "run `ployz init --force`",
		})
		return issues
	}

	switch networkPhase {
	case "running":
		// operational
	case "starting", "stopping":
		issues = append(issues, StatusIssue{
			Component: "runtime",
			Phase:     networkPhase,
			Message:   "network runtime is transitioning",
			Hint:      "wait for runtime convergence and retry",
		})
	case "stopped", "failed", "purged":
		issues = append(issues, StatusIssue{
			Component: "runtime",
			Phase:     networkPhase,
			Message:   "network runtime is not operational",
			Hint:      "run `ployz init --force`",
		})
	default:
		issues = append(issues, StatusIssue{
			Component: "runtime",
			Phase:     phaseOrUnknown(networkPhase),
			Message:   "network runtime phase is unknown",
			Hint:      "check daemon status and logs",
		})
	}

	if !s.WireGuard {
		issues = append(issues, StatusIssue{
			Component: "wireguard",
			Phase:     phaseFromHealthy(s.WireGuard),
			Message:   "wireguard is not ready",
			Hint:      "run `sudo ployz configure` and retry `ployz init --force`",
		})
	}

	if !s.Corrosion {
		issues = append(issues, StatusIssue{
			Component: "corrosion",
			Phase:     phaseFromHealthy(s.Corrosion),
			Message:   "corrosion is not ready",
			Hint:      "check daemon and corrosion logs, then retry",
		})
	}

	if s.DockerRequired && !s.DockerNet {
		issues = append(issues, StatusIssue{
			Component: "docker",
			Phase:     phaseFromHealthy(s.DockerNet),
			Message:   "docker network is not ready",
			Hint:      "ensure docker is running and retry `ployz init --force`",
		})
	}

	return issues
}

// WarningIssues returns non-blocking problems that still need operator attention.
func (s NetworkStatus) WarningIssues() []StatusIssue {
	issues := make([]StatusIssue, 0, 4)

	supervisorPhase := strings.TrimSpace(s.SupervisorPhase)
	switch supervisorPhase {
	case "", "running":
		// healthy or expected steady-state.
	case "degraded":
		issue := StatusIssue{
			Component: "supervisor",
			Phase:     phaseOrUnknown(supervisorPhase),
			Message:   "supervisor is degraded",
			Hint:      "check daemon logs for supervisor failures",
		}
		if strings.TrimSpace(s.SupervisorError) != "" {
			issue.Message = s.SupervisorError
		}
		issues = append(issues, issue)
	}

	clockPhase := strings.TrimSpace(s.ClockPhase)
	switch clockPhase {
	case "", "healthy":
		// no warning
	case "unchecked":
		issues = append(issues, StatusIssue{
			Component: "clock",
			Phase:     clockPhase,
			Message:   "clock synchronization has not been checked yet",
			Hint:      "wait for NTP check loop to run",
		})
	case "unhealthy_offset":
		issues = append(issues, StatusIssue{
			Component: "clock",
			Phase:     clockPhase,
			Message:   "clock offset exceeds threshold",
			Hint:      "ensure NTP is configured and synchronized",
		})
	case "error":
		msg := "NTP check failed"
		if strings.TrimSpace(s.ClockHealth.NTPError) != "" {
			msg = s.ClockHealth.NTPError
		}
		issues = append(issues, StatusIssue{
			Component: "clock",
			Phase:     clockPhase,
			Message:   msg,
			Hint:      "ensure NTP daemon access and connectivity",
		})
	default:
		issues = append(issues, StatusIssue{
			Component: "clock",
			Phase:     phaseOrUnknown(clockPhase),
			Message:   "clock health phase is unknown",
			Hint:      "check daemon logs",
		})
	}

	return issues
}

// ServiceReady reports whether service workloads can be scheduled safely.
func (s NetworkStatus) ServiceReady() bool {
	return len(s.ServiceBlockerIssues()) == 0
}

// ControlPlaneBlockerIssues returns blockers for control-plane operations
// that require supervisor convergence (node membership and orchestration flows).
func (s NetworkStatus) ControlPlaneBlockerIssues() []StatusIssue {
	issues := append([]StatusIssue(nil), s.ServiceBlockerIssues()...)

	supervisorPhase := phaseOrUnknown(s.SupervisorPhase)
	switch supervisorPhase {
	case "running", "degraded":
		// operational enough for control-plane workflows.
	default:
		issue := StatusIssue{
			Component: "supervisor",
			Phase:     supervisorPhase,
			Message:   "supervisor is not in a runnable state",
			Hint:      "check daemon logs for supervisor failures and backoff loops",
		}
		if msg := strings.TrimSpace(s.SupervisorError); msg != "" {
			issue.Message = msg
		}
		issues = append(issues, issue)
	}

	return issues
}

// ControlPlaneReady reports whether control-plane workflows can run safely.
func (s NetworkStatus) ControlPlaneReady() bool {
	return len(s.ControlPlaneBlockerIssues()) == 0
}

func phaseFromHealthy(healthy bool) string {
	if healthy {
		return "ready"
	}
	return "unready"
}

func phaseOrUnknown(phase string) string {
	phase = strings.TrimSpace(phase)
	if phase == "" {
		return "unknown"
	}
	return phase
}
