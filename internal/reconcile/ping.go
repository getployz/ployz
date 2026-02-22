package reconcile

import (
	"context"
	"log/slog"
	"net"
	"sync"
	"time"
)

const pingDialTimeout = 3 * time.Second

// PingTracker periodically TCP-dials peers and records connect RTT.
type PingTracker struct {
	mu   sync.RWMutex
	rtts map[string]time.Duration // nodeID → RTT, -1 = unreachable

	// DialFunc overrides TCP dialing for testing. If nil, real TCP dial is used.
	DialFunc func(ctx context.Context, addr string) (time.Duration, error)
}

// NewPingTracker creates a PingTracker ready to run.
func NewPingTracker() *PingTracker {
	return &PingTracker{
		rtts: make(map[string]time.Duration),
	}
}

// Run measures TCP connect time to all peers every interval.
// resolveAddrs is called each cycle and returns nodeID → "host:port".
func (pt *PingTracker) Run(ctx context.Context, selfID string, interval time.Duration, resolveAddrs func() map[string]string) {
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
		rtt    time.Duration
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
			rtt := pt.dial(ctx, a)
			ch <- result{nodeID: id, rtt: rtt}
		}(nodeID, addr)
	}

	go func() {
		wg.Wait()
		close(ch)
	}()

	pt.mu.Lock()
	for r := range ch {
		pt.rtts[r.nodeID] = r.rtt
	}
	pt.mu.Unlock()
}

func (pt *PingTracker) dial(ctx context.Context, addr string) time.Duration {
	if pt.DialFunc != nil {
		rtt, err := pt.DialFunc(ctx, addr)
		if err != nil {
			return -1
		}
		return rtt
	}
	return tcpPing(ctx, addr)
}

func tcpPing(ctx context.Context, addr string) time.Duration {
	dialCtx, cancel := context.WithTimeout(ctx, pingDialTimeout)
	defer cancel()

	start := time.Now()
	var d net.Dialer
	conn, err := d.DialContext(dialCtx, "tcp", addr)
	if err != nil {
		return -1
	}
	rtt := time.Since(start)
	_ = conn.Close()
	return rtt
}

// Snapshot returns the latest RTT per peer. -1 means unreachable.
func (pt *PingTracker) Snapshot() map[string]time.Duration {
	pt.mu.RLock()
	defer pt.mu.RUnlock()

	out := make(map[string]time.Duration, len(pt.rtts))
	for id, rtt := range pt.rtts {
		out[id] = rtt
	}
	return out
}

// Remove deletes a peer from the tracker.
func (pt *PingTracker) Remove(nodeID string) {
	pt.mu.Lock()
	delete(pt.rtts, nodeID)
	pt.mu.Unlock()
}
