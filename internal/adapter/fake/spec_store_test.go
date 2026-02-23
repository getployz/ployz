package fake

import (
	"errors"
	"testing"

	"ployz/pkg/sdk/types"
)

func TestSpecStore_SaveGetListDelete(t *testing.T) {
	store := NewSpecStore()
	spec := types.NetworkSpec{Network: "net-a", DataRoot: "/tmp/net-a", WGPort: 51820}

	if err := store.SaveSpec(spec, true); err != nil {
		t.Fatalf("SaveSpec() error = %v", err)
	}

	got, ok, err := store.GetSpec("net-a")
	if err != nil {
		t.Fatalf("GetSpec() error = %v", err)
	}
	if !ok {
		t.Fatal("GetSpec() ok=false, want true")
	}
	if got.Spec.Network != "net-a" || !got.Enabled {
		t.Fatalf("GetSpec() = %+v, want net-a enabled", got)
	}

	rows, err := store.ListSpecs()
	if err != nil {
		t.Fatalf("ListSpecs() error = %v", err)
	}
	if len(rows) != 1 || rows[0].Spec.Network != "net-a" {
		t.Fatalf("ListSpecs() = %+v, want one net-a row", rows)
	}

	if err := store.DeleteSpec("net-a"); err != nil {
		t.Fatalf("DeleteSpec() error = %v", err)
	}

	_, ok, err = store.GetSpec("net-a")
	if err != nil {
		t.Fatalf("GetSpec() after delete error = %v", err)
	}
	if ok {
		t.Fatal("GetSpec() after delete ok=true, want false")
	}
}

func TestSpecStore_ErrorInjection(t *testing.T) {
	store := NewSpecStore()
	injected := errors.New("injected")

	store.SaveSpecErr = func(types.NetworkSpec, bool) error { return injected }
	err := store.SaveSpec(types.NetworkSpec{Network: "net-a"}, true)
	if !errors.Is(err, injected) {
		t.Fatalf("SaveSpec() error = %v, want injected", err)
	}

	store.SaveSpecErr = nil
	if err := store.SaveSpec(types.NetworkSpec{Network: "net-a"}, true); err != nil {
		t.Fatalf("SaveSpec() setup error = %v", err)
	}

	store.GetSpecErr = func(string) error { return injected }
	_, _, err = store.GetSpec("net-a")
	if !errors.Is(err, injected) {
		t.Fatalf("GetSpec() error = %v, want injected", err)
	}
}

func TestSpecStore_FaultFailOnce(t *testing.T) {
	store := NewSpecStore()
	injected := errors.New("injected")
	store.FailOnce(FaultSpecStoreSaveSpec, injected)

	spec := types.NetworkSpec{Network: "net-a"}
	err := store.SaveSpec(spec, true)
	if !errors.Is(err, injected) {
		t.Fatalf("first SaveSpec() error = %v, want injected", err)
	}

	err = store.SaveSpec(spec, true)
	if err != nil {
		t.Fatalf("second SaveSpec() error = %v, want nil", err)
	}

	got, ok, err := store.GetSpec("net-a")
	if err != nil || !ok {
		t.Fatalf("GetSpec() ok=%v err=%v", ok, err)
	}
	if got.Spec.Network != "net-a" {
		t.Fatalf("GetSpec().Spec.Network = %q, want net-a", got.Spec.Network)
	}
}

func TestSpecStore_FaultHook(t *testing.T) {
	store := NewSpecStore()
	injected := errors.New("hook injected")
	var seenNetwork string

	store.SetFaultHook(FaultSpecStoreSaveSpec, func(args ...any) error {
		if len(args) != 2 {
			t.Fatalf("hook args len = %d, want 2", len(args))
		}
		spec, ok := args[0].(types.NetworkSpec)
		if !ok {
			t.Fatalf("hook arg[0] type = %T, want types.NetworkSpec", args[0])
		}
		seenNetwork = spec.Network
		if spec.Network == "net-a" {
			return injected
		}
		return nil
	})

	err := store.SaveSpec(types.NetworkSpec{Network: "net-a"}, true)
	if !errors.Is(err, injected) {
		t.Fatalf("SaveSpec(net-a) error = %v, want injected", err)
	}
	if seenNetwork != "net-a" {
		t.Fatalf("hook seen network = %q, want net-a", seenNetwork)
	}

	err = store.SaveSpec(types.NetworkSpec{Network: "net-b"}, true)
	if err != nil {
		t.Fatalf("SaveSpec(net-b) error = %v, want nil", err)
	}

	_, ok, err := store.GetSpec("net-a")
	if err != nil {
		t.Fatalf("GetSpec(net-a) error = %v", err)
	}
	if ok {
		t.Fatal("GetSpec(net-a) ok=true, want false")
	}

	got, ok, err := store.GetSpec("net-b")
	if err != nil {
		t.Fatalf("GetSpec(net-b) error = %v", err)
	}
	if !ok || got.Spec.Network != "net-b" {
		t.Fatalf("GetSpec(net-b) = %+v (ok=%v), want net-b", got, ok)
	}
}
