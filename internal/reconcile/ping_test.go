package reconcile

import (
	"context"
	"testing"
	"time"
)

func TestPingTracker_Snapshot(t *testing.T) {
	t.Run("empty tracker returns empty map", func(t *testing.T) {
		pt := NewPingTracker()
		snap := pt.Snapshot()
		if len(snap) != 0 {
			t.Fatalf("expected empty snapshot, got %d entries", len(snap))
		}
	})

	t.Run("snapshot contains probed peers", func(t *testing.T) {
		pt := NewPingTracker()
		pt.DialFunc = func(ctx context.Context, addr string) (time.Duration, error) {
			return 5 * time.Millisecond, nil
		}

		ctx, cancel := context.WithCancel(context.Background())
		done := make(chan struct{})
		go func() {
			pt.Run(ctx, "self", 50*time.Millisecond, func() map[string]string {
				return map[string]string{"peer1": "1.2.3.4:5000"}
			})
			close(done)
		}()

		// Let at least one probe cycle complete.
		time.Sleep(100 * time.Millisecond)
		snap := pt.Snapshot()
		cancel()
		<-done

		if _, ok := snap["peer1"]; !ok {
			t.Fatal("expected peer1 in snapshot")
		}
		if snap["peer1"] != 5*time.Millisecond {
			t.Fatalf("expected RTT 5ms, got %v", snap["peer1"])
		}
	})
}

func TestPingTracker_Remove(t *testing.T) {
	t.Run("remove non-existent entry does not panic", func(t *testing.T) {
		pt := NewPingTracker()
		pt.Remove("nonexistent")

		snap := pt.Snapshot()
		if len(snap) != 0 {
			t.Fatalf("expected empty snapshot, got %d entries", len(snap))
		}
	})

	t.Run("remove existing entry", func(t *testing.T) {
		pt := NewPingTracker()
		pt.DialFunc = func(ctx context.Context, addr string) (time.Duration, error) {
			return 5 * time.Millisecond, nil
		}

		ctx, cancel := context.WithCancel(context.Background())
		done := make(chan struct{})
		go func() {
			pt.Run(ctx, "self", 50*time.Millisecond, func() map[string]string {
				return map[string]string{"peer1": "1.2.3.4:5000"}
			})
			close(done)
		}()

		time.Sleep(100 * time.Millisecond)
		cancel()
		<-done

		snap := pt.Snapshot()
		if _, ok := snap["peer1"]; !ok {
			t.Fatal("expected peer1 in snapshot before remove")
		}

		pt.Remove("peer1")
		snap = pt.Snapshot()
		if len(snap) != 0 {
			t.Fatalf("expected 0 entries after remove, got %d", len(snap))
		}
	})
}
