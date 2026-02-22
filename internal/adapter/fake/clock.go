package fake

import (
	"sync"
	"time"

	"ployz/internal/network"
)

var _ network.Clock = (*Clock)(nil)

// Clock is a deterministic clock for testing.
type Clock struct {
	mu  sync.Mutex
	now time.Time
}

// NewClock creates a Clock starting at the given time.
func NewClock(start time.Time) *Clock {
	return &Clock{now: start}
}

// Now returns the current fake time.
func (c *Clock) Now() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.now
}

// Advance moves the clock forward by d.
func (c *Clock) Advance(d time.Duration) {
	c.mu.Lock()
	c.now = c.now.Add(d)
	c.mu.Unlock()
}

// Set sets the clock to an exact time.
func (c *Clock) Set(t time.Time) {
	c.mu.Lock()
	c.now = t
	c.mu.Unlock()
}
