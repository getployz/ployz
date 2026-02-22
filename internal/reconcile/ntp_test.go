package reconcile

import (
	"context"
	"testing"
	"time"
)

func TestNTPChecker_Status_Initial(t *testing.T) {
	clk := newTestClock(time.Now())
	nc := NewNTPChecker(clk)

	s := nc.Status()
	if s.Offset != 0 {
		t.Errorf("initial Offset: got %v, want 0", s.Offset)
	}
	if s.Healthy {
		t.Error("initial Healthy: got true, want false")
	}
	if s.Error != "" {
		t.Errorf("initial Error: got %q, want empty", s.Error)
	}
	if !s.CheckedAt.IsZero() {
		t.Errorf("initial CheckedAt: got %v, want zero", s.CheckedAt)
	}
}

func TestNTPChecker_Check_Healthy(t *testing.T) {
	t0 := time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC)
	clk := newTestClock(t0)
	nc := NewNTPChecker(clk)

	smallOffset := 10 * time.Millisecond
	nc.CheckFunc = func() NTPStatus {
		return NTPStatus{
			Offset:    smallOffset,
			Healthy:   true,
			CheckedAt: clk.Now(),
		}
	}

	ctx, cancel := context.WithCancel(context.Background())
	cancel() // cancel immediately so Run exits after the first check
	nc.Run(ctx)

	s := nc.Status()
	if !s.Healthy {
		t.Error("expected Healthy=true for small offset")
	}
	if s.Offset != smallOffset {
		t.Errorf("Offset: got %v, want %v", s.Offset, smallOffset)
	}
	if s.CheckedAt != t0 {
		t.Errorf("CheckedAt: got %v, want %v", s.CheckedAt, t0)
	}
}

func TestNTPChecker_Check_Unhealthy(t *testing.T) {
	t0 := time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC)
	clk := newTestClock(t0)
	nc := NewNTPChecker(clk)

	largeOffset := 2 * time.Second
	nc.CheckFunc = func() NTPStatus {
		return NTPStatus{
			Offset:    largeOffset,
			Healthy:   false,
			CheckedAt: clk.Now(),
		}
	}

	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	nc.Run(ctx)

	s := nc.Status()
	if s.Healthy {
		t.Error("expected Healthy=false for large offset")
	}
	if s.Offset != largeOffset {
		t.Errorf("Offset: got %v, want %v", s.Offset, largeOffset)
	}
}
