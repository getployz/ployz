package reconcile

import (
	"context"
	"sync"
	"time"

	"ployz/internal/check"
	"ployz/internal/mesh"

	"github.com/beevik/ntp"
)

const (
	defaultNTPPool      = "pool.ntp.org"
	defaultNTPInterval  = 60 * time.Second
	defaultNTPThreshold = 500 * time.Millisecond
)

type NTPStatus struct {
	Offset    time.Duration
	Healthy   bool
	Error     string
	CheckedAt time.Time
}

type NTPChecker struct {
	mu        sync.RWMutex
	status    NTPStatus
	pool      string
	interval  time.Duration
	threshold time.Duration
	clock     mesh.Clock

	// CheckFunc overrides real NTP queries for testing.
	// When set, check() calls this instead of ntp.Query().
	CheckFunc func() NTPStatus
}

func NewNTPChecker(clock mesh.Clock) *NTPChecker {
	check.Assert(clock != nil, "NewNTPChecker: clock must not be nil")
	return &NTPChecker{
		pool:      defaultNTPPool,
		interval:  defaultNTPInterval,
		threshold: defaultNTPThreshold,
		clock:     clock,
	}
}

func (n *NTPChecker) Run(ctx context.Context) {
	n.check()

	ticker := time.NewTicker(n.interval)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			n.check()
		}
	}
}

func (n *NTPChecker) check() {
	if n.CheckFunc != nil {
		n.mu.Lock()
		n.status = n.CheckFunc()
		n.mu.Unlock()
		return
	}

	resp, err := ntp.Query(n.pool)

	n.mu.Lock()
	defer n.mu.Unlock()

	now := n.clock.Now()
	if err != nil {
		n.status = NTPStatus{
			Error:     err.Error(),
			Healthy:   false,
			CheckedAt: now,
		}
		return
	}

	n.status = NTPStatus{
		Offset:    resp.ClockOffset,
		Healthy:   resp.ClockOffset.Abs() < n.threshold,
		CheckedAt: now,
	}
}

func (n *NTPChecker) Status() NTPStatus {
	n.mu.RLock()
	defer n.mu.RUnlock()
	return n.status
}
