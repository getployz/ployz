package convergence

import (
	"context"
	"log/slog"
	"net"
	"sync"
	"time"

	"ployz/internal/support/check"
)

const pingDialTimeout = 3 * time.Second

type PingPhase uint8

const (
	PingNoData PingPhase = iota + 1
	PingReachable
	PingUnreachable
)

func (p PingPhase) String() string {
	switch p {
	case PingNoData:
		return "no_data"
	case PingReachable:
		return "reachable"
	case PingUnreachable:
		return "unreachable"
	default:
		return "unknown"
	}
}

func (p PingPhase) Transition(to PingPhase) PingPhase {
	ok := false
	switch p {
	case PingNoData:
		ok = to == PingReachable || to == PingUnreachable
	case PingReachable:
		ok = to == PingUnreachable
	case PingUnreachable:
		ok = to == PingReachable
	}
	check.Assertf(ok, "ping transition: %s -> %s", p, to)
	if !ok {
		return p
	}
	return to
}

type Sample struct {
	Phase PingPhase
	RTT   time.Duration
}

type PingTracker struct {
	mu   sync.RWMutex
	rtts map[string]Sample

	DialFunc func(ctx context.Context, addr string) (time.Duration, error)
}

func NewPingTracker() *PingTracker {
	return &PingTracker{rtts: make(map[string]Sample)}
}

func (pt *PingTracker) Run(ctx context.Context, selfID string, interval time.Duration, resolveAddrs func() map[string]string) {
	check.Assert(resolveAddrs != nil, "ping.Tracker.Run: resolveAddrs must not be nil")
	check.Assert(interval > 0, "ping.Tracker.Run: interval must be positive")
	log := slog.With("component", "ping-tracker")
	log.Debug("starting", "interval", interval)

	ticker := time.NewTicker(interval)
	defer ticker.Stop()

	for {
		addrs := resolveAddrs()
		if len(addrs) > 0 {
			pt.probeAll(ctx, selfID, addrs)
		}

		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		}
	}
}

func (pt *PingTracker) probeAll(ctx context.Context, selfID string, addrs map[string]string) {
	type result struct {
		nodeID string
		sample Sample
	}

	ch := make(chan result, len(addrs))
	var wg sync.WaitGroup

	for nodeID, addr := range addrs {
		if nodeID == selfID {
			continue
		}
		wg.Add(1)
		go func(id, a string) {
			defer wg.Done()
			sample := pt.dial(ctx, a)
			ch <- result{nodeID: id, sample: sample}
		}(nodeID, addr)
	}

	go func() {
		wg.Wait()
		close(ch)
	}()

	pt.mu.Lock()
	for r := range ch {
		pt.rtts[r.nodeID] = r.sample
	}
	pt.mu.Unlock()
}

func (pt *PingTracker) dial(ctx context.Context, addr string) Sample {
	if pt.DialFunc != nil {
		rtt, err := pt.DialFunc(ctx, addr)
		if err != nil {
			return Sample{Phase: PingUnreachable}
		}
		if rtt < 0 {
			rtt = 0
		}
		return Sample{Phase: PingReachable, RTT: rtt}
	}
	rtt, err := tcpPing(ctx, addr)
	if err != nil {
		return Sample{Phase: PingUnreachable}
	}
	if rtt < 0 {
		rtt = 0
	}
	return Sample{Phase: PingReachable, RTT: rtt}
}

func tcpPing(ctx context.Context, addr string) (time.Duration, error) {
	dialCtx, cancel := context.WithTimeout(ctx, pingDialTimeout)
	defer cancel()

	start := time.Now()
	var d net.Dialer
	conn, err := d.DialContext(dialCtx, "tcp", addr)
	if err != nil {
		return 0, err
	}
	rtt := time.Since(start)
	_ = conn.Close()
	return rtt, nil
}

func (pt *PingTracker) Snapshot() map[string]Sample {
	pt.mu.RLock()
	defer pt.mu.RUnlock()

	out := make(map[string]Sample, len(pt.rtts))
	for id, sample := range pt.rtts {
		out[id] = sample
	}
	return out
}

func (pt *PingTracker) Remove(nodeID string) {
	pt.mu.Lock()
	delete(pt.rtts, nodeID)
	pt.mu.Unlock()
}
