package manager

import (
	"context"
	"net/netip"
	"path/filepath"

	"ployz/internal/infra/corrosion"
	"ployz/internal/infra/platform"
	"ployz/internal/infra/sqlite"
	"ployz/internal/daemon/convergence"
	"ployz/internal/daemon/membership"
	"ployz/internal/daemon/overlay"
	"ployz/internal/daemon/workload"
	"ployz/pkg/sdk/defaults"
	"ployz/pkg/sdk/types"
)

// NewProduction creates a Manager with real platform, storage, and registry adapters.
func NewProduction(ctx context.Context, dataRoot string) (*Manager, error) {
	dataRoot = normalizedDataRoot(dataRoot)

	var cfg managerCfg
	if err := initPlatformDefaults(ctx, dataRoot, &cfg); err != nil {
		return nil, err
	}

	m, err := New(ctx, dataRoot,
		WithSpecStore(cfg.specStore),
		WithManagerStateStore(cfg.stateStore),
		WithOverlayService(cfg.overlay),
		WithMembershipService(cfg.membership),
		WithConvergenceService(cfg.convergence),
		WithWorkloadService(cfg.workload),
	)
	if err != nil {
		_ = cfg.overlay.Close()   // best-effort cleanup
		_ = cfg.specStore.Close() // best-effort cleanup
		return nil, err
	}

	return m, nil
}

// initPlatformDefaults fills any nil fields on cfg with real platform
// implementations backed by SQLite, Corrosion, and the platform controller.
func initPlatformDefaults(ctx context.Context, dataRoot string, cfg *managerCfg) error {
	if err := defaults.EnsureDataRoot(dataRoot); err != nil {
		return err
	}

	statePath := filepath.Join(dataRoot, "daemon.db")
	sqlStore, err := sqlite.Open(statePath)
	if err != nil {
		return err
	}
	cfg.specStore = &sqliteSpecStore{s: sqlStore}

	registryFactory := overlay.RegistryFactory(func(addr netip.AddrPort, token string) overlay.Registry {
		return corrosion.NewStore(addr, token)
	})

	netStateStore := sqlite.NetworkStateStore{}
	cfg.stateStore = netStateStore

	ctrl, err := platform.NewController(overlay.WithRegistryFactory(registryFactory))
	if err != nil {
		_ = cfg.specStore.Close() // best-effort cleanup
		return err
	}
	cfg.overlay = ctrl
	cfg.membership = membership.New(ctrl)
	cfg.workload = workload.New()

	cfg.convergence = convergence.New(ctx,
		convergence.WithControllerFactory(func() (convergence.NetworkController, error) {
			return platform.NewController(overlay.WithRegistryFactory(registryFactory))
		}),
		convergence.WithPeerReconcilerFactory(func() (convergence.PeerReconciler, error) {
			return platform.NewController(overlay.WithRegistryFactory(registryFactory))
		}),
		convergence.WithRegistryFactory(func(addr netip.AddrPort, token string) convergence.Registry {
			return corrosion.NewStore(addr, token)
		}),
		convergence.WithStateStore(netStateStore),
	)

	if err := startPlatformServices(ctx); err != nil {
		_ = ctrl.Close()          // best-effort cleanup
		_ = cfg.specStore.Close() // best-effort cleanup
		return err
	}

	return nil
}

// sqliteSpecStore adapts *sqlite.Store to the SpecStore interface.
type sqliteSpecStore struct {
	s *sqlite.Store
}

func (a *sqliteSpecStore) SaveSpec(spec types.NetworkSpec, enabled bool) error {
	return a.s.SaveSpec(spec, enabled)
}

func (a *sqliteSpecStore) GetSpec() (PersistedSpec, bool, error) {
	p, ok, err := a.s.GetSpec()
	if err != nil || !ok {
		return PersistedSpec{}, ok, err
	}
	return PersistedSpec{Spec: p.Spec, Enabled: p.Enabled}, true, nil
}

func (a *sqliteSpecStore) DeleteSpec() error {
	return a.s.DeleteSpec()
}

func (a *sqliteSpecStore) Close() error {
	return a.s.Close()
}
