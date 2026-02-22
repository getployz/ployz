package fake

import (
	"errors"
	"os"
	"testing"

	"ployz/internal/network"
)

func TestStateStore_SaveLoad(t *testing.T) {
	ss := NewStateStore()

	state := &network.State{Network: "test-net", Subnet: "10.0.0.0/24"}
	if err := ss.Save("/data/test-net", state); err != nil {
		t.Fatal(err)
	}

	loaded, err := ss.Load("/data/test-net")
	if err != nil {
		t.Fatal(err)
	}
	if loaded.Network != "test-net" {
		t.Errorf("expected network 'test-net', got %q", loaded.Network)
	}

	// Verify deep copy — mutating loaded shouldn't affect stored.
	loaded.Network = "mutated"
	reloaded, err := ss.Load("/data/test-net")
	if err != nil {
		t.Fatal(err)
	}
	if reloaded.Network != "test-net" {
		t.Errorf("deep copy failed: stored state was mutated to %q", reloaded.Network)
	}
}

func TestStateStore_LoadMissing(t *testing.T) {
	ss := NewStateStore()
	_, err := ss.Load("/nonexistent")
	if !errors.Is(err, os.ErrNotExist) {
		t.Errorf("expected os.ErrNotExist, got %v", err)
	}
}

func TestStateStore_Delete(t *testing.T) {
	ss := NewStateStore()
	_ = ss.Save("/data/test", &network.State{Network: "test"})
	if err := ss.Delete("/data/test"); err != nil {
		t.Fatal(err)
	}
	_, err := ss.Load("/data/test")
	if !errors.Is(err, os.ErrNotExist) {
		t.Errorf("expected os.ErrNotExist after delete, got %v", err)
	}
}

func TestStateStore_ErrorInjection(t *testing.T) {
	ss := NewStateStore()
	injected := errors.New("disk full")

	ss.SaveErr = func(string, *network.State) error { return injected }
	if err := ss.Save("/data/x", &network.State{}); !errors.Is(err, injected) {
		t.Errorf("expected injected error, got %v", err)
	}

	ss.SaveErr = nil
	_ = ss.Save("/data/x", &network.State{})

	ss.LoadErr = func(string) error { return injected }
	_, err := ss.Load("/data/x")
	if !errors.Is(err, injected) {
		t.Errorf("expected injected error, got %v", err)
	}
}

func TestStateStore_StatePath(t *testing.T) {
	ss := NewStateStore()
	if got := ss.StatePath("/data/test"); got != "fake:///data/test" {
		t.Errorf("unexpected state path: %q", got)
	}
}

func TestStateStore_CallRecording(t *testing.T) {
	ss := NewStateStore()
	_ = ss.Save("/data/a", &network.State{})
	_, _ = ss.Load("/data/a")
	_ = ss.Delete("/data/a")

	if len(ss.Calls("Save")) != 1 {
		t.Errorf("expected 1 Save call, got %d", len(ss.Calls("Save")))
	}
	if len(ss.Calls("Load")) != 1 {
		t.Errorf("expected 1 Load call, got %d", len(ss.Calls("Load")))
	}
	if len(ss.Calls("Delete")) != 1 {
		t.Errorf("expected 1 Delete call, got %d", len(ss.Calls("Delete")))
	}
}
