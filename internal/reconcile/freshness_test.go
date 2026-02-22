package reconcile

import (
	"sync"
	"testing"
	"time"
)

// testClock is a minimal deterministic clock for freshness tests.
// Inline stub avoids an import cycle with adapter/fake.
type testClock struct {
	mu  sync.Mutex
	now time.Time
}

func newTestClock(start time.Time) *testClock {
	return &testClock{now: start}
}

func (c *testClock) Now() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.now
}

func (c *testClock) Advance(d time.Duration) {
	c.mu.Lock()
	c.now = c.now.Add(d)
	c.mu.Unlock()
}

func TestFreshnessTracker_RecordSeen(t *testing.T) {
	t0 := time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC)

	tests := []struct {
		name       string
		selfID     string
		recordID   string
		recordAt   time.Time
		wantInSnap bool
	}{
		{
			name:       "new peer appears in snapshot",
			selfID:     "self",
			recordID:   "peer-1",
			recordAt:   t0,
			wantInSnap: true,
		},
		{
			name:       "self ID is ignored",
			selfID:     "self",
			recordID:   "self",
			recordAt:   t0,
			wantInSnap: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			clk := newTestClock(t0)
			ft := NewFreshnessTracker(tt.selfID, clk)

			ft.RecordSeen(tt.recordID, tt.recordAt)

			snap := ft.Snapshot()
			_, found := snap[tt.recordID]
			if found != tt.wantInSnap {
				t.Fatalf("peer %q in snapshot: got %v, want %v", tt.recordID, found, tt.wantInSnap)
			}
		})
	}

	t.Run("overwrite existing peer with new timestamp", func(t *testing.T) {
		clk := newTestClock(t0)
		ft := NewFreshnessTracker("self", clk)

		ft.RecordSeen("peer-1", t0)
		clk.Advance(1 * time.Second)

		// Record again at the new clock time.
		ft.RecordSeen("peer-1", t0.Add(1*time.Second))

		// Snapshot taken immediately after the second RecordSeen, so freshness should be ~0.
		snap := ft.Snapshot()
		ph, ok := snap["peer-1"]
		if !ok {
			t.Fatal("peer-1 missing from snapshot after overwrite")
		}
		if ph.Freshness != 0 {
			t.Fatalf("freshness after overwrite: got %v, want 0", ph.Freshness)
		}
	})
}

func TestFreshnessTracker_Remove(t *testing.T) {
	t0 := time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC)

	t.Run("remove existing peer", func(t *testing.T) {
		clk := newTestClock(t0)
		ft := NewFreshnessTracker("self", clk)

		ft.RecordSeen("peer-1", t0)
		ft.Remove("peer-1")

		snap := ft.Snapshot()
		if _, ok := snap["peer-1"]; ok {
			t.Fatal("peer-1 should not be in snapshot after removal")
		}
	})

	t.Run("remove non-existent peer does not panic", func(t *testing.T) {
		clk := newTestClock(t0)
		ft := NewFreshnessTracker("self", clk)

		// Should not panic.
		ft.Remove("does-not-exist")

		snap := ft.Snapshot()
		if len(snap) != 0 {
			t.Fatalf("snapshot should be empty, got %d entries", len(snap))
		}
	})
}

func TestFreshnessTracker_Snapshot(t *testing.T) {
	t0 := time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC)

	tests := []struct {
		name          string
		advance       time.Duration   // time to advance after RecordSeen
		reportedAt    time.Time       // updatedAt passed to RecordSeen
		peers         []string        // peers to record (empty = test empty tracker)
		wantStale     map[string]bool // expected Stale per peer
		wantFreshness map[string]time.Duration
		wantLag       map[string]time.Duration
	}{
		{
			name:          "empty tracker returns empty map",
			peers:         nil,
			wantStale:     map[string]bool{},
			wantFreshness: map[string]time.Duration{},
			wantLag:       map[string]time.Duration{},
		},
		{
			name:          "one peer fresh within staleAge",
			peers:         []string{"peer-1"},
			reportedAt:    t0,
			advance:       1 * time.Second,
			wantStale:     map[string]bool{"peer-1": false},
			wantFreshness: map[string]time.Duration{"peer-1": 1 * time.Second},
			wantLag:       map[string]time.Duration{"peer-1": 0},
		},
		{
			name:          "one peer stale past staleAge",
			peers:         []string{"peer-1"},
			reportedAt:    t0,
			advance:       5 * time.Second,
			wantStale:     map[string]bool{"peer-1": true},
			wantFreshness: map[string]time.Duration{"peer-1": 5 * time.Second},
			wantLag:       map[string]time.Duration{"peer-1": 0},
		},
		{
			name:       "negative replication lag clamped to zero",
			peers:      []string{"peer-1"},
			reportedAt: t0.Add(10 * time.Second), // reported time is in the future relative to local clock
			advance:    0,
			wantStale:  map[string]bool{"peer-1": false},
			wantLag:    map[string]time.Duration{"peer-1": 0},
		},
		{
			name:          "boundary at exactly staleAge is not stale",
			peers:         []string{"peer-1"},
			reportedAt:    t0,
			advance:       defaultStaleAge, // exactly 3s
			wantStale:     map[string]bool{"peer-1": false},
			wantFreshness: map[string]time.Duration{"peer-1": defaultStaleAge},
			wantLag:       map[string]time.Duration{"peer-1": 0},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			clk := newTestClock(t0)
			ft := NewFreshnessTracker("self", clk)

			for _, id := range tt.peers {
				ft.RecordSeen(id, tt.reportedAt)
			}
			clk.Advance(tt.advance)

			snap := ft.Snapshot()

			if len(snap) != len(tt.peers) {
				t.Fatalf("snapshot length: got %d, want %d", len(snap), len(tt.peers))
			}

			for id, wantStale := range tt.wantStale {
				ph, ok := snap[id]
				if !ok {
					t.Fatalf("peer %q missing from snapshot", id)
				}
				if ph.Stale != wantStale {
					t.Errorf("peer %q Stale: got %v, want %v", id, ph.Stale, wantStale)
				}
			}

			for id, wantFresh := range tt.wantFreshness {
				ph := snap[id]
				if ph.Freshness != wantFresh {
					t.Errorf("peer %q Freshness: got %v, want %v", id, ph.Freshness, wantFresh)
				}
			}

			for id, wantLag := range tt.wantLag {
				ph := snap[id]
				if ph.ReplicationLag != wantLag {
					t.Errorf("peer %q ReplicationLag: got %v, want %v", id, ph.ReplicationLag, wantLag)
				}
			}
		})
	}
}
