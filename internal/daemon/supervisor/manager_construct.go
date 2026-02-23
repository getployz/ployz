package supervisor

import (
	"context"
	"fmt"
	"log/slog"
	"strings"

	"ployz/pkg/sdk/defaults"
)

func New(ctx context.Context, dataRoot string, opts ...ManagerOption) (*Manager, error) {
	log := slog.With("component", "supervisor")
	dataRoot = normalizedDataRoot(dataRoot)

	var cfg managerCfg
	for _, o := range opts {
		o(&cfg)
	}

	if err := validateManagerConfig(cfg); err != nil {
		return nil, err
	}

	m := &Manager{
		ctx:        ctx,
		dataRoot:   dataRoot,
		store:      cfg.specStore,
		stateStore: cfg.stateStore,
		ctrl:       cfg.ctrl,
		engine:     cfg.eng,
	}

	m.restoreEnabledNetworks(ctx, dataRoot, log)
	m.stopOnContextDone(ctx, log)

	return m, nil
}

func normalizedDataRoot(dataRoot string) string {
	if strings.TrimSpace(dataRoot) == "" {
		return defaults.DataRoot()
	}
	return dataRoot
}

func (m *Manager) restoreEnabledNetworks(ctx context.Context, dataRoot string, log *slog.Logger) {
	// Start workers for all enabled specs.
	specs, err := m.store.ListSpecs()
	if err != nil {
		return
	}

	for _, item := range specs {
		if !item.Enabled {
			continue
		}
		network := defaults.NormalizeNetwork(item.Spec.Network)
		if network == "" {
			continue
		}
		item.Spec.Network = network
		if item.Spec.DataRoot == "" {
			item.Spec.DataRoot = dataRoot
		}
		log.Info("restoring enabled network", "network", network)
		if startErr := m.engine.StartNetwork(ctx, item.Spec); startErr != nil {
			log.Warn("failed to restore network worker", "network", network, "err", startErr)
		}
	}
}

func (m *Manager) stopOnContextDone(ctx context.Context, log *slog.Logger) {
	go func() {
		<-ctx.Done()
		log.Info("stopping")
		m.engine.StopAll()
		_ = m.ctrl.Close()  // best-effort cleanup
		_ = m.store.Close() // best-effort cleanup
	}()
}

func validateManagerConfig(cfg managerCfg) error {
	if cfg.specStore == nil {
		return fmt.Errorf("spec store is required")
	}
	if cfg.stateStore == nil {
		return fmt.Errorf("state store is required")
	}
	if cfg.ctrl == nil {
		return fmt.Errorf("controller is required")
	}
	if cfg.eng == nil {
		return fmt.Errorf("engine is required")
	}
	return nil
}
