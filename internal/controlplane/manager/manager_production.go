package manager

import (
	"context"
	"net/netip"
	"path/filepath"

	"ployz/internal/adapter/corrosion"
	"ployz/internal/adapter/platform"
	"ployz/internal/adapter/sqlite"
	"ployz/internal/engine"
	"ployz/internal/network"
	"ployz/internal/reconcile"
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
		WithManagerController(cfg.ctrl),
		WithManagerEngine(cfg.eng),
	)
	if err != nil {
		_ = cfg.ctrl.Close()      // best-effort cleanup
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

	registryFactory := network.RegistryFactory(func(addr netip.AddrPort, token string) network.Registry {
		return corrosion.NewStore(addr, token)
	})
	netStateStore := sqlite.NetworkStateStore{}
	cfg.stateStore = netStateStore

	ctrl, err := platform.NewController(network.WithRegistryFactory(registryFactory))
	if err != nil {
		_ = cfg.specStore.Close() // best-effort cleanup
		return err
	}
	cfg.ctrl = ctrl

	cfg.eng = engine.New(ctx,
		engine.WithControllerFactory(func() (engine.NetworkController, error) {
			return platform.NewController(network.WithRegistryFactory(registryFactory))
		}),
		engine.WithPeerReconcilerFactory(func() (reconcile.PeerReconciler, error) {
			return platform.NewController(network.WithRegistryFactory(registryFactory))
		}),
		engine.WithRegistryFactory(func(addr netip.AddrPort, token string) reconcile.Registry {
			return corrosion.NewStore(addr, token)
		}),
		engine.WithStateStore(netStateStore),
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
