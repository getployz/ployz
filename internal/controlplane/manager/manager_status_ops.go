package manager

import (
	"context"

	"ployz/internal/signal/freshness"
	ntpsignal "ployz/internal/signal/ntp"
	"ployz/internal/signal/ping"
	"ployz/pkg/sdk/types"
)

func (m *Manager) GetStatus(ctx context.Context) (types.NetworkStatus, error) {
	_, cfg, err := m.resolveConfig()
	if err != nil {
		return types.NetworkStatus{}, err
	}

	status, err := m.ctrl.Status(ctx, cfg)
	if err != nil {
		return types.NetworkStatus{}, err
	}

	phase, _ := m.engine.Status()
	health := m.engine.Health()

	return types.NetworkStatus{
		Configured:    status.Configured,
		Running:       status.Running,
		WireGuard:     status.WireGuard,
		Corrosion:     status.Corrosion,
		DockerNet:     status.DockerNet,
		StatePath:     status.StatePath,
		WorkerRunning: workerPhaseRunning(phase),
		ClockHealth:   clockHealth(health.NTPStatus),
	}, nil
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
