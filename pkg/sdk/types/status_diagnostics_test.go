package types

import "testing"

func TestServiceBlockerIssuesUnconfigured(t *testing.T) {
	st := NetworkStatus{}
	issues := st.ServiceBlockerIssues()
	if len(issues) != 1 {
		t.Fatalf("expected 1 blocker, got %d", len(issues))
	}
	if issues[0].Component != "config" {
		t.Fatalf("expected config blocker, got %q", issues[0].Component)
	}
	if st.ServiceReady() {
		t.Fatal("expected service not ready")
	}
}

func TestServiceBlockerIssuesDockerRequired(t *testing.T) {
	st := NetworkStatus{
		Configured:     true,
		Running:        true,
		WireGuard:      true,
		Corrosion:      true,
		DockerNet:      false,
		NetworkPhase:   "running",
		DockerRequired: true,
	}
	issues := st.ServiceBlockerIssues()
	if len(issues) != 1 {
		t.Fatalf("expected 1 blocker, got %d", len(issues))
	}
	if issues[0].Component != "docker" {
		t.Fatalf("expected docker blocker, got %q", issues[0].Component)
	}
	if st.ServiceReady() {
		t.Fatal("expected service not ready")
	}
}

func TestServiceReadyWhenRuntimeHealthy(t *testing.T) {
	st := NetworkStatus{
		Configured:     true,
		Running:        true,
		WireGuard:      true,
		Corrosion:      true,
		DockerNet:      true,
		NetworkPhase:   "running",
		DockerRequired: true,
	}
	if blockers := st.ServiceBlockerIssues(); len(blockers) != 0 {
		t.Fatalf("expected no blockers, got %+v", blockers)
	}
	if !st.ServiceReady() {
		t.Fatal("expected service ready")
	}
}

func TestWarningIssues(t *testing.T) {
	st := NetworkStatus{
		SupervisorPhase: "degraded",
		SupervisorError: "subscription failed",
		ClockPhase:      "error",
		ClockHealth: ClockHealth{
			NTPError: "dns lookup failed",
		},
	}
	issues := st.WarningIssues()
	if len(issues) != 2 {
		t.Fatalf("expected 2 warnings, got %d", len(issues))
	}
	if issues[0].Component != "supervisor" {
		t.Fatalf("expected supervisor warning, got %q", issues[0].Component)
	}
	if issues[1].Component != "clock" {
		t.Fatalf("expected clock warning, got %q", issues[1].Component)
	}
}

func TestControlPlaneBlockerIssuesIncludesSupervisor(t *testing.T) {
	st := NetworkStatus{
		Configured:        true,
		Running:           true,
		WireGuard:         true,
		Corrosion:         true,
		DockerNet:         true,
		NetworkPhase:      "running",
		DockerRequired:    true,
		SupervisorPhase:   "backoff",
		SupervisorError:   "machine subscription failed",
		SupervisorRunning: true,
	}

	if !st.ServiceReady() {
		t.Fatal("expected service readiness to remain true")
	}

	blockers := st.ControlPlaneBlockerIssues()
	if len(blockers) != 1 {
		t.Fatalf("expected 1 control-plane blocker, got %d", len(blockers))
	}
	if blockers[0].Component != "supervisor" {
		t.Fatalf("expected supervisor blocker, got %q", blockers[0].Component)
	}
	if st.ControlPlaneReady() {
		t.Fatal("expected control-plane readiness to be false")
	}
}
