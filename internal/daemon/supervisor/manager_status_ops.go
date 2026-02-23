package supervisor

import (
	"context"

	"ployz/internal/reconcile"
	"ployz/pkg/sdk/defaults"
	"ployz/pkg/sdk/types"
)

func (m *Manager) GetStatus(ctx context.Context, network string) (types.NetworkStatus, error) {
	network = defaults.NormalizeNetwork(network)
	_, cfg, err := m.resolveConfig(network)
	if err != nil {
		return types.NetworkStatus{}, err
	}

	status, err := m.ctrl.Status(ctx, cfg)
	if err != nil {
		return types.NetworkStatus{}, err
	}

	running, _ := m.engine.Status(network)
	health := m.engine.Health(network)

	return types.NetworkStatus{
		Configured:    status.Configured,
		Running:       status.Running,
		WireGuard:     status.WireGuard,
		Corrosion:     status.Corrosion,
		DockerNet:     status.DockerNet,
		StatePath:     status.StatePath,
		WorkerRunning: running,
		ClockHealth:   clockHealth(health.NTPStatus),
	}, nil
}

func (m *Manager) GetPeerHealth(ctx context.Context, network string) ([]types.PeerHealthResponse, error) {
	network = defaults.NormalizeNetwork(network)
	health := m.engine.Health(network)

	// Determine self ID.
	selfID := ""
	if identity, err := m.GetIdentity(ctx, network); err == nil {
		selfID = identity.ID
	}

	peers := make([]types.PeerLag, 0, len(health.Peers))
	for nodeID, ph := range health.Peers {
		peers = append(peers, types.PeerLag{
			NodeID:         nodeID,
			Freshness:      ph.Freshness,
			Stale:          ph.Stale,
			ReplicationLag: ph.ReplicationLag,
			PingRTT:        ph.PingRTT,
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

func clockHealth(ntp reconcile.NTPStatus) types.ClockHealth {
	return types.ClockHealth{
		NTPOffsetMs: float64(ntp.Offset.Milliseconds()),
		NTPHealthy:  ntp.Healthy,
		NTPError:    ntp.Error,
	}
}
