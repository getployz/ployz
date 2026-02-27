package manager

import (
	"context"
	"errors"
	"runtime"
	"strings"

	"ployz/internal/health/freshness"
	ntpsignal "ployz/internal/health/ntp"
	"ployz/internal/health/ping"
	"ployz/pkg/sdk/types"
)

func (m *Manager) GetStatus(ctx context.Context) (types.NetworkStatus, error) {
	supervisorPhase, supervisorErr := m.engine.Status()
	health := m.engine.Health()
	dockerRequired := runtime.GOOS == "linux"

	base := types.NetworkStatus{
		DockerRequired:    dockerRequired,
		SupervisorPhase:   supervisorPhase.String(),
		SupervisorError:   strings.TrimSpace(supervisorErr),
		SupervisorRunning: supervisorPhaseRunning(supervisorPhase),
		ClockHealth:       clockHealth(health.NTPStatus),
		ClockPhase:        health.NTPStatus.Phase.String(),
	}

	_, cfg, err := m.resolveConfig()
	if errors.Is(err, ErrNetworkNotConfigured) {
		base.Configured = false
		base.Running = false
		base.WireGuard = false
		base.Corrosion = false
		base.DockerNet = !dockerRequired
		base.StatePath = ""
		base.NetworkPhase = "unconfigured"
		base.RuntimeTree = buildRuntimeTree(base)
		return base, nil
	}
	if err != nil {
		return types.NetworkStatus{}, err
	}

	status, err := m.ctrl.Status(ctx, cfg)
	if err != nil {
		return types.NetworkStatus{}, err
	}

	base.Configured = status.Configured
	base.Running = status.Running
	base.WireGuard = status.WireGuard
	base.Corrosion = status.Corrosion
	base.DockerNet = status.DockerNet
	base.StatePath = status.StatePath
	base.NetworkPhase = normalizedNetworkPhase(status.Phase)
	base.RuntimeTree = buildRuntimeTree(base)

	return base, nil
}

func (m *Manager) GetPeerHealth(ctx context.Context) ([]types.PeerHealthResponse, error) {
	health := m.engine.Health()

	// Determine self ID.
	selfID := ""
	if identity, err := m.GetIdentity(ctx); err == nil {
		selfID = identity.ID
	}

	peers := make([]types.PeerLag, 0, len(health.Peers))
	for nodeID, ph := range health.Peers {
		pingRTT := ph.PingRTT
		if ph.PingPhase == ping.PingUnreachable {
			pingRTT = -1
		}
		peers = append(peers, types.PeerLag{
			NodeID:         nodeID,
			Freshness:      ph.Freshness,
			Stale:          ph.Phase == freshness.FreshnessStale,
			ReplicationLag: ph.ReplicationLag,
			PingRTT:        pingRTT,
		})
	}

	return []types.PeerHealthResponse{
		{
			NodeID: selfID,
			NTP:    clockHealth(health.NTPStatus),
			Peers:  peers,
		},
	}, nil
}

func clockHealth(ntp ntpsignal.Status) types.ClockHealth {
	return types.ClockHealth{
		NTPOffsetMs: float64(ntp.Offset.Milliseconds()),
		NTPHealthy:  ntp.Phase == ntpsignal.NTPHealthy,
		NTPError:    ntp.Error,
	}
}

func normalizedNetworkPhase(phase string) string {
	trimmed := strings.TrimSpace(phase)
	if trimmed == "" {
		return "unknown"
	}
	return trimmed
}

func buildRuntimeTree(st types.NetworkStatus) types.StateNode {
	root := types.StateNode{
		Component: "runtime",
		Phase:     normalizedNetworkPhase(st.NetworkPhase),
		Required:  true,
		Healthy:   len(st.ServiceBlockerIssues()) == 0,
		Children:  make([]types.StateNode, 0, 7),
	}

	configNode := types.StateNode{
		Component: "config",
		Phase:     boolPhase(st.Configured, "configured", "unconfigured"),
		Required:  true,
		Healthy:   st.Configured,
		Hint:      "run `ployz network create default --force`",
	}
	if !configNode.Healthy {
		configNode.LastErrorCode = "network.unconfigured"
		configNode.LastError = "network is not configured"
	}
	root.Children = append(root.Children, configNode)

	networkHealthy := st.Running && st.NetworkPhase == "running"
	networkNode := types.StateNode{
		Component: "network",
		Phase:     normalizedNetworkPhase(st.NetworkPhase),
		Required:  true,
		Healthy:   networkHealthy,
		Hint:      "wait for convergence or re-run `ployz network create default --force`",
	}
	if !networkNode.Healthy {
		networkNode.LastErrorCode = "network.phase.not_running"
		networkNode.LastError = "network runtime is not in running phase"
	}
	root.Children = append(root.Children, networkNode)

	wireguardNode := types.StateNode{
		Component: "wireguard",
		Phase:     boolPhase(st.WireGuard, "ready", "unready"),
		Required:  true,
		Healthy:   st.WireGuard,
		Hint:      "run `sudo ployz configure` and `ployz network create default --force`",
	}
	if !wireguardNode.Healthy {
		wireguardNode.LastErrorCode = "wireguard.not_ready"
		wireguardNode.LastError = "wireguard interface is not active"
	}
	root.Children = append(root.Children, wireguardNode)

	corrosionNode := types.StateNode{
		Component: "corrosion",
		Phase:     boolPhase(st.Corrosion, "ready", "unready"),
		Required:  true,
		Healthy:   st.Corrosion,
		Hint:      "check daemon and corrosion logs",
	}
	if !corrosionNode.Healthy {
		corrosionNode.LastErrorCode = "corrosion.not_ready"
		corrosionNode.LastError = "corrosion runtime is not healthy"
	}
	root.Children = append(root.Children, corrosionNode)

	dockerNode := types.StateNode{
		Component: "docker",
		Required:  st.DockerRequired,
		Healthy:   !st.DockerRequired || st.DockerNet,
		Hint:      "ensure docker daemon is running",
	}
	if st.DockerRequired {
		dockerNode.Phase = boolPhase(st.DockerNet, "ready", "unready")
		if !dockerNode.Healthy {
			dockerNode.LastErrorCode = "docker.network.not_ready"
			dockerNode.LastError = "docker network is not ready"
		}
	} else {
		dockerNode.Phase = "not_required"
	}
	root.Children = append(root.Children, dockerNode)

	supervisorHealthy := st.SupervisorPhase == "running" || st.SupervisorPhase == "degraded"
	supervisorNode := types.StateNode{
		Component: "supervisor",
		Phase:     phaseOrUnknown(st.SupervisorPhase),
		Required:  false,
		Healthy:   supervisorHealthy,
		Hint:      "check daemon logs for supervisor errors",
	}
	if strings.TrimSpace(st.SupervisorError) != "" {
		supervisorNode.LastErrorCode = "supervisor.error"
		supervisorNode.LastError = st.SupervisorError
	}
	root.Children = append(root.Children, supervisorNode)

	clockHealthy := st.ClockPhase == "healthy" || st.ClockPhase == "unchecked" || st.ClockPhase == ""
	clockNode := types.StateNode{
		Component: "clock",
		Phase:     phaseOrUnknown(st.ClockPhase),
		Required:  false,
		Healthy:   clockHealthy,
		Hint:      "ensure NTP is configured",
	}
	if !clockNode.Healthy {
		clockNode.LastErrorCode = "clock.not_synced"
		clockNode.LastError = strings.TrimSpace(st.ClockHealth.NTPError)
		if clockNode.LastError == "" {
			clockNode.LastError = "clock synchronization is unhealthy"
		}
	}
	root.Children = append(root.Children, clockNode)

	return root
}

func boolPhase(ok bool, okPhase, badPhase string) string {
	if ok {
		return okPhase
	}
	return badPhase
}

func phaseOrUnknown(phase string) string {
	trimmed := strings.TrimSpace(phase)
	if trimmed == "" {
		return "unknown"
	}
	return trimmed
}
