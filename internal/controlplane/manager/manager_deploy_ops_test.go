package manager

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	"ployz/internal/adapter/sqlite"
	"ployz/internal/network"
	"ployz/internal/observed"
	"ployz/pkg/sdk/types"
)

func TestReadContainerStateFallsBackToLocalObservedCache(t *testing.T) {
	t.Parallel()

	store := &fakeSpecStore{
		spec: PersistedSpec{Spec: types.NetworkSpec{Network: "testnet"}, Enabled: true},
		ok:   true,
	}
	runtime := &fakeContainerRuntime{listErr: errors.New("docker unavailable")}

	mgr := newTestManager(t, store, runtime, func(context.Context, network.Config) (int, []string, error) {
		return 0, nil, nil
	}, func(context.Context, network.Config) (int, []string, error) {
		return 0, nil, nil
	})
	runtimeStore := sqlite.RuntimeContainerStore{}
	mgr.runtimeStore = runtimeStore

	_, cfg, err := mgr.resolveConfig()
	if err != nil {
		t.Fatalf("resolve config: %v", err)
	}

	seed := []observed.ContainerRecord{{
		MachineID:     "machine-1",
		Namespace:     "shop",
		DeployID:      "deploy-1",
		ContainerName: "web-1",
		Image:         "nginx:1.27",
		Running:       true,
		Healthy:       true,
	}}
	if err := runtimeStore.ReplaceNamespaceSnapshot(context.Background(), cfg.DataDir, "machine-1", "shop", seed, time.Now().UTC()); err != nil {
		t.Fatalf("seed runtime cache: %v", err)
	}

	out, err := mgr.ReadContainerState(context.Background(), "shop")
	if err != nil {
		t.Fatalf("read container state: %v", err)
	}
	if len(out) != 1 {
		t.Fatalf("expected 1 container state row, got %d", len(out))
	}
	if out[0].ContainerName != "web-1" {
		t.Fatalf("expected container web-1, got %q", out[0].ContainerName)
	}
	if !out[0].Running {
		t.Fatalf("expected running=true")
	}
	if !out[0].Healthy {
		t.Fatalf("expected healthy=true")
	}
}

func TestReadContainerStateReturnsRuntimeErrorWhenCacheMissing(t *testing.T) {
	t.Parallel()

	store := &fakeSpecStore{
		spec: PersistedSpec{Spec: types.NetworkSpec{Network: "testnet"}, Enabled: true},
		ok:   true,
	}
	runtime := &fakeContainerRuntime{listErr: errors.New("docker unavailable")}

	mgr := newTestManager(t, store, runtime, func(context.Context, network.Config) (int, []string, error) {
		return 0, nil, nil
	}, func(context.Context, network.Config) (int, []string, error) {
		return 0, nil, nil
	})
	mgr.runtimeStore = sqlite.RuntimeContainerStore{}

	_, err := mgr.ReadContainerState(context.Background(), "shop")
	if err == nil {
		t.Fatalf("expected error when runtime and cache are unavailable")
	}
	if !strings.Contains(err.Error(), "docker unavailable") {
		t.Fatalf("expected runtime error in returned error, got %v", err)
	}
}

func TestReadContainerStatePersistsRuntimeNamespaceCursor(t *testing.T) {
	t.Parallel()

	store := &fakeSpecStore{
		spec: PersistedSpec{Spec: types.NetworkSpec{Network: "testnet"}, Enabled: true},
		ok:   true,
	}
	runtime := &fakeContainerRuntime{entries: []network.ContainerListEntry{{
		Name:    "web-1",
		Image:   "nginx:1.27",
		Running: true,
		Labels: map[string]string{
			managedLabelNamespace: "shop",
			managedLabelDeployID:  "deploy-1",
		},
	}}}

	mgr := newTestManager(t, store, runtime, func(context.Context, network.Config) (int, []string, error) {
		return 0, nil, nil
	}, func(context.Context, network.Config) (int, []string, error) {
		return 0, nil, nil
	})
	mgr.stateStore = staticStateStore{state: &network.State{WGPublic: "machine-1"}}
	runtimeStore := sqlite.RuntimeContainerStore{}
	cursorStore := sqlite.RuntimeCursorStore{}
	mgr.runtimeStore = runtimeStore
	mgr.runtimeCursorStore = cursorStore

	_, cfg, err := mgr.resolveConfig()
	if err != nil {
		t.Fatalf("resolve config: %v", err)
	}

	if _, err := mgr.ReadContainerState(context.Background(), "shop"); err != nil {
		t.Fatalf("read container state: %v", err)
	}

	value, ok, err := cursorStore.GetCursor(context.Background(), cfg.DataDir, runtimeNamespaceCursorName("shop"))
	if err != nil {
		t.Fatalf("get runtime cursor: %v", err)
	}
	if !ok {
		t.Fatalf("expected runtime cursor to be stored")
	}
	if strings.TrimSpace(value) == "" {
		t.Fatalf("expected non-empty runtime cursor value")
	}
}

func TestClearRuntimeNamespaceSnapshotRemovesRowsAndPersistsCursor(t *testing.T) {
	t.Parallel()

	store := &fakeSpecStore{
		spec: PersistedSpec{Spec: types.NetworkSpec{Network: "testnet"}, Enabled: true},
		ok:   true,
	}
	runtime := &fakeContainerRuntime{}

	mgr := newTestManager(t, store, runtime, func(context.Context, network.Config) (int, []string, error) {
		return 0, nil, nil
	}, func(context.Context, network.Config) (int, []string, error) {
		return 0, nil, nil
	})
	mgr.stateStore = staticStateStore{state: &network.State{WGPublic: "machine-1"}}
	runtimeStore := sqlite.RuntimeContainerStore{}
	cursorStore := sqlite.RuntimeCursorStore{}
	mgr.runtimeStore = runtimeStore
	mgr.runtimeCursorStore = cursorStore

	_, cfg, err := mgr.resolveConfig()
	if err != nil {
		t.Fatalf("resolve config: %v", err)
	}

	seed := []observed.ContainerRecord{{
		MachineID:     "machine-1",
		Namespace:     "shop",
		ContainerName: "web-1",
		Image:         "nginx:1.27",
		Running:       true,
		Healthy:       true,
	}}
	if err := runtimeStore.ReplaceNamespaceSnapshot(context.Background(), cfg.DataDir, "machine-1", "shop", seed, time.Now().UTC()); err != nil {
		t.Fatalf("seed runtime cache: %v", err)
	}

	if err := mgr.clearRuntimeNamespaceSnapshot(context.Background(), cfg.DataDir, "machine-1", "shop"); err != nil {
		t.Fatalf("clear runtime namespace snapshot: %v", err)
	}

	rows, err := runtimeStore.ListNamespace(context.Background(), cfg.DataDir, "shop")
	if err != nil {
		t.Fatalf("list runtime namespace rows: %v", err)
	}
	if len(rows) != 0 {
		t.Fatalf("expected empty namespace snapshot after clear, got %d rows", len(rows))
	}

	value, ok, err := cursorStore.GetCursor(context.Background(), cfg.DataDir, runtimeNamespaceCursorName("shop"))
	if err != nil {
		t.Fatalf("get runtime cursor after clear: %v", err)
	}
	if !ok {
		t.Fatalf("expected runtime cursor after clear")
	}
	if strings.TrimSpace(value) == "" {
		t.Fatalf("expected non-empty runtime cursor value after clear")
	}
}

type staticStateStore struct {
	state *network.State
}

func (s staticStateStore) Load(string) (*network.State, error) {
	if s.state == nil {
		return nil, errors.New("missing test state")
	}
	return s.state, nil
}

func (staticStateStore) Save(string, *network.State) error { return nil }

func (staticStateStore) Delete(string) error { return nil }

func (staticStateStore) StatePath(string) string { return "" }
