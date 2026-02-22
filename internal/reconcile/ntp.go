package reconcile

import (
	"context"
	"sync"
	"time"

	"ployz/internal/network"

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
	clock     network.Clock
}

func NewNTPChecker(clock network.Clock) *NTPChecker {
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

	offset := resp.ClockOffset
	if offset < 0 {
		offset = -offset
	}

	n.status = NTPStatus{
		Offset:    resp.ClockOffset,
		Healthy:   offset < n.threshold,
		CheckedAt: now,
	}
}

func (n *NTPChecker) Status() NTPStatus {
	n.mu.RLock()
	defer n.mu.RUnlock()
	return n.status
}
