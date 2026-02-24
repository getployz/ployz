package ntp

import (
	"context"
	"sync"
	"time"

	"ployz/internal/check"
	"ployz/internal/network"

	"github.com/beevik/ntp"
)

const (
	defaultNTPPool      = "pool.ntp.org"
	defaultNTPInterval  = 60 * time.Second
	defaultNTPThreshold = 500 * time.Millisecond
)

type NTPPhase uint8

const (
	NTPUnchecked NTPPhase = iota + 1
	NTPHealthy
	NTPUnhealthyOffset
	NTPError
)

func (p NTPPhase) String() string {
	switch p {
	case NTPUnchecked:
		return "unchecked"
	case NTPHealthy:
		return "healthy"
	case NTPUnhealthyOffset:
		return "unhealthy_offset"
	case NTPError:
		return "error"
	default:
		return "unknown"
	}
}

func (p NTPPhase) Transition(to NTPPhase) NTPPhase {
	ok := false
	switch p {
	case NTPUnchecked:
		ok = to == NTPHealthy || to == NTPUnhealthyOffset || to == NTPError
	case NTPHealthy:
		ok = to == NTPUnhealthyOffset || to == NTPError
	case NTPUnhealthyOffset:
		ok = to == NTPHealthy || to == NTPError
	case NTPError:
		ok = to == NTPHealthy || to == NTPUnhealthyOffset || to == NTPError
	}
	check.Assertf(ok, "ntp transition: %s -> %s", p, to)
	if !ok {
		return p
	}
	return to
}

type Status struct {
	Offset    time.Duration
	Phase     NTPPhase
	Error     string
	CheckedAt time.Time
}

type Checker struct {
	mu        sync.RWMutex
	status    Status
	pool      string
	interval  time.Duration
	threshold time.Duration
	clock     network.Clock

	CheckFunc func() Status
}

func NewChecker(clock network.Clock) *Checker {
	check.Assert(clock != nil, "ntp.NewChecker: clock must not be nil")
	return &Checker{
		pool:      defaultNTPPool,
		interval:  defaultNTPInterval,
		threshold: defaultNTPThreshold,
		status: Status{
			Phase: NTPUnchecked,
		},
		clock: clock,
	}
}

func (n *Checker) Run(ctx context.Context) {
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

func (n *Checker) check() {
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
		n.status = Status{Error: err.Error(), Phase: NTPError, CheckedAt: now}
		return
	}

	phase := NTPUnhealthyOffset
	if resp.ClockOffset.Abs() < n.threshold {
		phase = NTPHealthy
	}
	n.status = Status{Offset: resp.ClockOffset, Phase: phase, CheckedAt: now}
}

func (n *Checker) Status() Status {
	n.mu.RLock()
	defer n.mu.RUnlock()
	return n.status
}
