package engine

import (
	"context"
	"fmt"
	"log/slog"
	"sync"
	"time"

	"ployz/internal/check"
	netctrl "ployz/internal/mesh"
	"ployz/internal/reconcile"
	"ployz/pkg/sdk/defaults"
	"ployz/pkg/sdk/types"
)

const (
	// workerRetryDelay is 2s: short enough for quick recovery, long enough to avoid tight spin on persistent failures.
	workerRetryDelay = 2 * time.Second
	// workerMaxRetryDelay is 60s: caps exponential backoff to avoid excessive wait.
	workerMaxRetryDelay = 60 * time.Second
	// workerMaxConsecutiveFailures is 100: give up after sustained failure (~15 min at cap).
	workerMaxConsecutiveFailures = 100
	// stopNetworkTimeout is 30s: maximum time to wait for a worker goroutine to exit.
	stopNetworkTimeout = 30 * time.Second
)

type NetworkHealth struct {
	Peers     map[string]reconcile.PeerHealth
	NTPStatus reconcile.NTPStatus
}

type Engine struct {
	mu            sync.Mutex
	workers       map[string]*workerState
	rootCtx       context.Context
	newController NetworkControllerFactory
	newReconciler PeerReconcilerFactory
	newRegistry   RegistryFactory
	stateStore    netctrl.StateStore
	clock         netctrl.Clock
	pingDialFunc  func(context.Context, string) (time.Duration, error)
	ntpCheckFunc  func() reconcile.NTPStatus
}

type EngineOption func(*Engine)

func WithControllerFactory(f NetworkControllerFactory) EngineOption {
	return func(e *Engine) { e.newController = f }
}

func WithPeerReconcilerFactory(f PeerReconcilerFactory) EngineOption {
	return func(e *Engine) { e.newReconciler = f }
}

func WithRegistryFactory(f RegistryFactory) EngineOption {
	return func(e *Engine) { e.newRegistry = f }
}

func WithStateStore(s netctrl.StateStore) EngineOption {
	return func(e *Engine) { e.stateStore = s }
}

func WithClock(c netctrl.Clock) EngineOption {
	return func(e *Engine) { e.clock = c }
}

func WithPingDialFunc(f func(context.Context, string) (time.Duration, error)) EngineOption {
	return func(e *Engine) { e.pingDialFunc = f }
}

func WithNTPCheckFunc(f func() reconcile.NTPStatus) EngineOption {
	return func(e *Engine) { e.ntpCheckFunc = f }
}

func New(ctx context.Context, opts ...EngineOption) *Engine {
	e := &Engine{
		workers: make(map[string]*workerState),
		rootCtx: ctx,
	}
	for _, opt := range opts {
		opt(e)
	}
	check.Assert(e.newController != nil, "Engine.New: NetworkControllerFactory is required")
	check.Assert(e.newReconciler != nil, "Engine.New: PeerReconcilerFactory is required")
	check.Assert(e.newRegistry != nil, "Engine.New: RegistryFactory is required")
	return e
}

func (e *Engine) StartNetwork(ctx context.Context, spec types.NetworkSpec) error {
	network := defaults.NormalizeNetwork(spec.Network)
	if network == "" {
		return fmt.Errorf("network is required")
	}
	if err := e.rootCtx.Err(); err != nil {
		return fmt.Errorf("engine is shutting down: %w", err)
	}
	log := slog.With("component", "runtime-engine", "network", network)

	e.mu.Lock()
	defer e.mu.Unlock()

	if existing, ok := e.workers[network]; ok {
		log.Info("restarting worker")
		existing.cancel()
		<-existing.done
		delete(e.workers, network)
	}

	workerCtx, cancel := context.WithCancel(e.rootCtx)

	selfID := e.loadSelfID(spec)

	clk := e.clockOrDefault()

	ws := &workerState{
		cancel:    cancel,
		done:      make(chan struct{}),
		spec:      spec,
		freshness: reconcile.NewFreshnessTracker(selfID, clk),
		ntp:       reconcile.NewNTPChecker(clk),
		ping:      reconcile.NewPingTracker(),
	}
	if e.ntpCheckFunc != nil {
		ws.ntp.CheckFunc = e.ntpCheckFunc
	}
	if e.pingDialFunc != nil {
		ws.ping.DialFunc = e.pingDialFunc
	}
	e.workers[network] = ws
	log.Info("starting worker")

	go func() {
		defer close(ws.done)
		e.runWorkerLoop(workerCtx, ws, spec)
	}()

	return nil
}

func (e *Engine) StopNetwork(network string) error {
	network = defaults.NormalizeNetwork(network)
	log := slog.With("component", "runtime-engine", "network", network)

	e.mu.Lock()
	ws, ok := e.workers[network]
	if !ok {
		e.mu.Unlock()
		return nil
	}
	ws.cancel()
	delete(e.workers, network)
	e.mu.Unlock()

	select {
	case <-ws.done:
		log.Info("worker stopped")
	case <-time.After(stopNetworkTimeout):
		log.Warn("worker stop timed out")
	}
	return nil
}

func (e *Engine) Status(network string) (running bool, lastErr string) {
	network = defaults.NormalizeNetwork(network)

	e.mu.Lock()
	ws, ok := e.workers[network]
	e.mu.Unlock()

	if !ok {
		return false, ""
	}
	return ws.status()
}

func (e *Engine) Health(network string) NetworkHealth {
	network = defaults.NormalizeNetwork(network)

	e.mu.Lock()
	ws, ok := e.workers[network]
	e.mu.Unlock()

	if !ok {
		return NetworkHealth{}
	}

	peers := ws.freshness.Snapshot()

	// Merge ping RTTs into peer health.
	pings := ws.ping.Snapshot()
	if len(pings) > 0 && peers == nil {
		peers = make(map[string]reconcile.PeerHealth)
	}
	for nodeID, rtt := range pings {
		ph := peers[nodeID]
		ph.PingRTT = rtt
		peers[nodeID] = ph
	}

	return NetworkHealth{
		Peers:     peers,
		NTPStatus: ws.ntp.Status(),
	}
}

func (e *Engine) StopAll() {
	log := slog.With("component", "runtime-engine")

	e.mu.Lock()
	workers := e.workers
	e.workers = make(map[string]*workerState)
	e.mu.Unlock()

	if len(workers) == 0 {
		return
	}

	log.Info("stopping all workers", "count", len(workers))
	for _, ws := range workers {
		ws.cancel()
	}
	for network, ws := range workers {
		select {
		case <-ws.done:
			log.Debug("worker stopped", "network", network)
		case <-time.After(stopNetworkTimeout):
			log.Warn("worker stop timed out", "network", network)
		}
	}
}

type workerState struct {
	cancel    context.CancelFunc
	done      chan struct{}
	spec      types.NetworkSpec
	freshness *reconcile.FreshnessTracker
	ntp       *reconcile.NTPChecker
	ping      *reconcile.PingTracker
	running   bool
	lastErr   string
	mu        sync.RWMutex
}

func (w *workerState) setStatus(running bool, lastErr string) {
	w.mu.Lock()
	w.running = running
	w.lastErr = lastErr
	w.mu.Unlock()
}

func (w *workerState) status() (bool, string) {
	w.mu.RLock()
	defer w.mu.RUnlock()
	return w.running, w.lastErr
}

func (e *Engine) runWorkerLoop(ctx context.Context, ws *workerState, spec types.NetworkSpec) {
	network := defaults.NormalizeNetwork(spec.Network)
	log := slog.With("component", "runtime-engine", "network", network)

	var consecutiveFailures int
	retryDelay := workerRetryDelay

	// backoff logs the error, updates status, sleeps, and bumps the failure counter.
	// Returns false if the context was cancelled or max failures reached.
	backoff := func(msg string, err error) bool {
		log.Debug(msg, "err", err)
		ws.setStatus(false, err.Error())
		if !sleepWithContext(ctx, retryDelay) {
			return false
		}
		consecutiveFailures++
		if consecutiveFailures >= workerMaxConsecutiveFailures {
			log.Error("worker giving up after too many consecutive failures", "failures", consecutiveFailures)
			ws.setStatus(false, fmt.Sprintf("gave up after %d consecutive failures: %v", consecutiveFailures, err))
			return false
		}
		retryDelay = min(retryDelay*2, workerMaxRetryDelay)
		return true
	}

	for {
		cfg, err := netctrl.ConfigFromSpec(spec)
		if err != nil {
			if !backoff("invalid network spec", err) {
				return
			}
			continue
		}

		startCtrl, err := e.newController()
		if err != nil {
			if !backoff("create controller failed", err) {
				return
			}
			continue
		}
		runtimeCfg, startErr := startCtrl.Start(ctx, cfg)
		_ = startCtrl.Close()
		if startErr != nil {
			if !backoff("start runtime failed", startErr) {
				return
			}
			continue
		}

		peerCtrl, err := e.newReconciler()
		if err != nil {
			if !backoff("create peer reconciler failed", err) {
				return
			}
			continue
		}

		reg := e.newRegistry(runtimeCfg.CorrosionAPIAddr, runtimeCfg.CorrosionAPIToken)

		// Reset failure tracking on successful setup.
		consecutiveFailures = 0
		retryDelay = workerRetryDelay
		ws.setStatus(true, "")
		log.Debug("runtime prepared, entering reconcile loop")

		worker := reconcile.Worker{
			Spec:           runtimeCfg,
			Registry:       reg,
			PeerReconciler: peerCtrl,
			StateStore:     e.stateStore,
			Freshness:      ws.freshness,
			NTP:            ws.ntp,
			Ping:           ws.ping,
			OnFailure: func(err error) {
				if err == nil {
					return
				}
				ws.setStatus(true, err.Error())
			},
		}

		err = worker.Run(ctx)
		_ = peerCtrl.Close()
		if ctx.Err() != nil {
			ws.setStatus(false, "")
			log.Debug("worker loop canceled")
			return
		}
		if err != nil {
			log.Warn("worker loop exited with error", "err", err)
			ws.setStatus(false, err.Error())
		} else {
			log.Debug("worker loop exited cleanly")
			ws.setStatus(false, "")
		}

		if !sleepWithContext(ctx, retryDelay) {
			return
		}
	}
}

func (e *Engine) clockOrDefault() netctrl.Clock {
	if e.clock != nil {
		return e.clock
	}
	return netctrl.RealClock{}
}

func (e *Engine) loadSelfID(spec types.NetworkSpec) string {
	if e.stateStore == nil {
		return ""
	}
	cfg, err := netctrl.ConfigFromSpec(spec)
	if err != nil {
		return ""
	}
	st, err := netctrl.LoadState(e.stateStore, cfg)
	if err != nil {
		return ""
	}
	return st.WGPublic
}

func sleepWithContext(ctx context.Context, d time.Duration) bool {
	timer := time.NewTimer(d)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return false
	case <-timer.C:
		return true
	}
}
