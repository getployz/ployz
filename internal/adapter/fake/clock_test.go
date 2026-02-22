package fake

import (
	"testing"
	"time"
)

func TestClock_Now(t *testing.T) {
	start := time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC)
	c := NewClock(start)

	if got := c.Now(); !got.Equal(start) {
		t.Errorf("expected %v, got %v", start, got)
	}
}

func TestClock_Advance(t *testing.T) {
	start := time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC)
	c := NewClock(start)

	c.Advance(5 * time.Second)
	want := start.Add(5 * time.Second)
	if got := c.Now(); !got.Equal(want) {
		t.Errorf("expected %v, got %v", want, got)
	}
}

func TestClock_Set(t *testing.T) {
	start := time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC)
	c := NewClock(start)

	target := time.Date(2026, 6, 15, 12, 0, 0, 0, time.UTC)
	c.Set(target)
	if got := c.Now(); !got.Equal(target) {
		t.Errorf("expected %v, got %v", target, got)
	}
}
