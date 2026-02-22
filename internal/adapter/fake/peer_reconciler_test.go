package fake

import (
	"context"
	"errors"
	"testing"

	"ployz/internal/network"
)

func TestPeerReconciler_ReconcilePeers(t *testing.T) {
	rec := NewPeerReconciler()
	ctx := context.Background()

	rows := []network.MachineRow{
		{ID: "m1", PublicKey: "pk1"},
		{ID: "m2", PublicKey: "pk2"},
	}

	n, err := rec.ReconcilePeers(ctx, network.Config{}, rows)
	if err != nil {
		t.Fatal(err)
	}
	if n != 2 {
		t.Errorf("expected 2, got %d", n)
	}
	if len(rec.LastRows) != 2 {
		t.Errorf("expected 2 captured rows, got %d", len(rec.LastRows))
	}
	if rec.LastRows[0].ID != "m1" {
		t.Errorf("expected first row ID 'm1', got %q", rec.LastRows[0].ID)
	}
}

func TestPeerReconciler_Close(t *testing.T) {
	rec := NewPeerReconciler()
	if err := rec.Close(); err != nil {
		t.Fatal(err)
	}
	if !rec.Closed {
		t.Error("expected Closed to be true")
	}
}

func TestPeerReconciler_ErrorInjection(t *testing.T) {
	rec := NewPeerReconciler()
	injected := errors.New("reconcile failed")

	rec.ReconcilePeersErr = func(context.Context, network.Config, []network.MachineRow) error {
		return injected
	}

	_, err := rec.ReconcilePeers(context.Background(), network.Config{}, nil)
	if !errors.Is(err, injected) {
		t.Errorf("expected injected error, got %v", err)
	}
}

func TestPeerReconciler_CallRecording(t *testing.T) {
	rec := NewPeerReconciler()
	ctx := context.Background()

	_, _ = rec.ReconcilePeers(ctx, network.Config{}, nil)
	_ = rec.Close()

	if len(rec.Calls("ReconcilePeers")) != 1 {
		t.Error("expected 1 ReconcilePeers call")
	}
	if len(rec.Calls("Close")) != 1 {
		t.Error("expected 1 Close call")
	}
}
