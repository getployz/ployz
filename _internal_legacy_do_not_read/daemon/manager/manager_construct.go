package manager

import (
	"context"
	"fmt"
	"log/slog"
	"strings"

	"ployz/pkg/sdk/defaults"
)

func New(ctx context.Context, dataRoot string, opts ...ManagerOption) (*Manager, error) {
	log := slog.With("component", "manager")
	dataRoot = normalizedDataRoot(dataRoot)

	var cfg managerCfg
	for _, o := range opts {
		o(&cfg)
	}

	if err := validateManagerConfig(cfg); err != nil {
		return nil, err
	}

	m := &Manager{
		ctx:                     ctx,
		dataRoot:                dataRoot,
		store:                   cfg.specStore,
		stateStore:              cfg.stateStore,
		overlay:                 cfg.overlay,
		membership:              cfg.membership,
		convergence:             cfg.convergence,
		workload:                cfg.workload,
		attachedMachinesSummary: cfg.attachedMachinesSummary,
	}

	m.restoreNetwork(ctx, dataRoot, log)
	m.stopOnContextDone(ctx, log)

	return m, nil
}

func normalizedDataRoot(dataRoot string) string {
	if strings.TrimSpace(dataRoot) == "" {
		return defaults.DataRoot()
	}
	return dataRoot
}

func (m *Manager) restoreNetwork(ctx context.Context, dataRoot string, log *slog.Logger) {
	persisted, ok, err := m.store.GetSpec()
	if err != nil || !ok || !persisted.Enabled {
		return
	}

	spec := persisted.Spec
	network := defaults.NormalizeNetwork(spec.Network)
	if network == "" {
		return
	}
	spec.Network = network
	if spec.DataRoot == "" {
		spec.DataRoot = dataRoot
	}

	log.Info("restoring enabled network", "network", network)
	if startErr := m.convergence.Start(ctx, spec); startErr != nil {
		log.Warn("failed to restore network worker", "network", network, "err", startErr)
	}
}

func (m *Manager) stopOnContextDone(ctx context.Context, log *slog.Logger) {
	go func() {
		<-ctx.Done()
		log.Info("stopping")
		m.convergence.StopAll()
		_ = m.membership.Close() // best-effort cleanup
		_ = m.overlay.Close()    // best-effort cleanup
		_ = m.store.Close()      // best-effort cleanup
	}()
}

func validateManagerConfig(cfg managerCfg) error {
	if cfg.specStore == nil {
		return fmt.Errorf("spec store is required")
	}
	if cfg.stateStore == nil {
		return fmt.Errorf("state store is required")
	}
	if cfg.overlay == nil {
		return fmt.Errorf("overlay service is required")
	}
	if cfg.membership == nil {
		return fmt.Errorf("membership service is required")
	}
	if cfg.convergence == nil {
		return fmt.Errorf("convergence service is required")
	}
	if cfg.workload == nil {
		return fmt.Errorf("workload service is required")
	}
	return nil
}
