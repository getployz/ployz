package supervisor

import (
	"fmt"
	"testing"

	"ployz/pkg/sdk/types"
)

type testSpecStore struct {
	items map[string]PersistedSpec
}

func newTestSpecStore() *testSpecStore {
	return &testSpecStore{items: make(map[string]PersistedSpec)}
}

func (s *testSpecStore) SaveSpec(spec types.NetworkSpec, enabled bool) error {
	s.items[spec.Network] = PersistedSpec{Spec: spec, Enabled: enabled}
	return nil
}

func (s *testSpecStore) GetSpec(network string) (PersistedSpec, bool, error) {
	p, ok := s.items[network]
	return p, ok, nil
}

func (s *testSpecStore) ListSpecs() ([]PersistedSpec, error) {
	out := make([]PersistedSpec, 0, len(s.items))
	for _, item := range s.items {
		out = append(out, item)
	}
	return out, nil
}

func (s *testSpecStore) DeleteSpec(network string) error {
	if _, ok := s.items[network]; !ok {
		return fmt.Errorf("not found")
	}
	delete(s.items, network)
	return nil
}

func (s *testSpecStore) Close() error { return nil }

func TestNormalizedDataRoot(t *testing.T) {
	t.Run("empty uses defaults", func(t *testing.T) {
		if got := normalizedDataRoot(""); got == "" {
			t.Fatal("expected non-empty default data root")
		}
	})

	t.Run("whitespace uses defaults", func(t *testing.T) {
		if got := normalizedDataRoot("   "); got == "" {
			t.Fatal("expected non-empty default data root")
		}
	})

	t.Run("explicit value preserved", func(t *testing.T) {
		const path = "/tmp/custom-data-root"
		if got := normalizedDataRoot(path); got != path {
			t.Fatalf("normalizedDataRoot() = %q, want %q", got, path)
		}
	})
}

func TestResolveSpec(t *testing.T) {
	t.Run("empty network errors", func(t *testing.T) {
		m := &Manager{store: newTestSpecStore(), dataRoot: "/tmp/default-root"}
		_, err := m.resolveSpec("")
		if err == nil {
			t.Fatal("expected error for empty network")
		}
	})

	t.Run("returns persisted spec with defaults applied", func(t *testing.T) {
		store := newTestSpecStore()
		if err := store.SaveSpec(types.NetworkSpec{Network: "demo"}, true); err != nil {
			t.Fatalf("seed spec store: %v", err)
		}

		m := &Manager{store: store, dataRoot: "/tmp/default-root"}
		spec, err := m.resolveSpec("demo")
		if err != nil {
			t.Fatalf("resolveSpec: %v", err)
		}
		if spec.Network != "demo" {
			t.Fatalf("spec.Network = %q, want demo", spec.Network)
		}
		if spec.DataRoot != "/tmp/default-root" {
			t.Fatalf("spec.DataRoot = %q, want /tmp/default-root", spec.DataRoot)
		}
	})

	t.Run("returns synthetic default when missing", func(t *testing.T) {
		m := &Manager{store: newTestSpecStore(), dataRoot: "/tmp/default-root"}
		spec, err := m.resolveSpec("missing")
		if err != nil {
			t.Fatalf("resolveSpec: %v", err)
		}
		if spec.Network != "missing" {
			t.Fatalf("spec.Network = %q, want missing", spec.Network)
		}
		if spec.DataRoot != "/tmp/default-root" {
			t.Fatalf("spec.DataRoot = %q, want /tmp/default-root", spec.DataRoot)
		}
	})
}
