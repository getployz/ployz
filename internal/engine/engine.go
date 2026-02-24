package engine

import (
	"context"
	"fmt"
	"log/slog"
	"sync"
	"time"

	"ployz/internal/check"
	"ployz/internal/network"
	"ployz/internal/reconcile"
	"ployz/internal/signal/freshness"
	"ployz/internal/signal/ntp"
	"ployz/internal/signal/ping"
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
	Peers     map[string]freshness.PeerHealth
	NTPStatus ntp.Status
}

type Engine struct {
	mu            sync.Mutex
	worker        *workerState
	rootCtx       context.Context
	newController NetworkControllerFactory
	newReconciler PeerReconcilerFactory
	newRegistry   RegistryFactory
	stateStore    network.StateStore
	clock         network.Clock
	pingDialFunc  func(context.Context, string) (time.Duration, error)
	ntpCheckFunc  func() ntp.Status
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

func WithStateStore(s network.StateStore) EngineOption {
	return func(e *Engine) { e.stateStore = s }
}

func WithClock(c network.Clock) EngineOption {
	return func(e *Engine) { e.clock = c }
}

func WithPingDialFunc(f func(context.Context, string) (time.Duration, error)) EngineOption {
	return func(e *Engine) { e.pingDialFunc = f }
}

func WithNTPCheckFunc(f func() ntp.Status) EngineOption {
	return func(e *Engine) { e.ntpCheckFunc = f }
}

func New(ctx context.Context, opts ...EngineOption) *Engine {
	e := &Engine{
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

func (e *Engine) Start(ctx context.Context, spec types.NetworkSpec) error {
	networkName := defaults.NormalizeNetwork(spec.Network)
	if networkName == "" {
		return fmt.Errorf("network is required")
	}
	if err := e.rootCtx.Err(); err != nil {
		return fmt.Errorf("engine is shutting down: %w", err)
	}
	log := slog.With("component", "runtime-engine", "network", networkName)

	e.mu.Lock()
	defer e.mu.Unlock()

	if existing := e.worker; existing != nil {
		log.Info("restarting worker")
		existing.cancel()
		<-existing.done
		e.worker = nil
	}

	workerCtx, cancel := context.WithCancel(e.rootCtx)

	selfID := e.loadSelfID(spec)

	clk := e.clockOrDefault()

	ws := &workerState{
		cancel:    cancel,
		done:      make(chan struct{}),
		spec:      spec,
		freshness: freshness.NewTracker(selfID, clk),
		ntp:       ntp.NewChecker(clk),
		ping:      ping.NewTracker(),
	}
	if e.ntpCheckFunc != nil {
		ws.ntp.CheckFunc = e.ntpCheckFunc
	}
	if e.pingDialFunc != nil {
		ws.ping.DialFunc = e.pingDialFunc
	}
	e.worker = ws
	log.Info("starting worker")

	go func() {
		defer close(ws.done)
		e.runWorkerLoop(workerCtx, ws, spec)
	}()

	return nil
}

func (e *Engine) Stop() error {
	networkName := ""
	e.mu.Lock()
	if e.worker != nil {
		networkName = defaults.NormalizeNetwork(e.worker.spec.Network)
	}
	e.mu.Unlock()
	log := slog.With("component", "runtime-engine", "network", networkName)

	e.mu.Lock()
	ws := e.worker
	if ws == nil {
		e.mu.Unlock()
		return nil
	}
	ws.cancel()
	e.worker = nil
	e.mu.Unlock()

	select {
	case <-ws.done:
		log.Info("worker stopped")
	case <-time.After(stopNetworkTimeout):
		log.Warn("worker stop timed out")
	}
	return nil
}

func (e *Engine) Status() (phase WorkerPhase, lastErr string) {
	e.mu.Lock()
	ws := e.worker
	e.mu.Unlock()

	if ws == nil {
		return WorkerAbsent, ""
	}
	return ws.status()
}

func (e *Engine) Health() NetworkHealth {
	e.mu.Lock()
	ws := e.worker
	e.mu.Unlock()

	if ws == nil {
		return NetworkHealth{}
	}

	peers := ws.freshness.Snapshot()

	// Merge ping RTTs into peer health.
	pings := ws.ping.Snapshot()
	if len(pings) > 0 && peers == nil {
		peers = make(map[string]freshness.PeerHealth)
	}
	for nodeID, sample := range pings {
		ph := peers[nodeID]
		ph.PingPhase = sample.Phase
		ph.PingRTT = sample.RTT
		peers[nodeID] = ph
	}

	return NetworkHealth{
		Peers:     peers,
		NTPStatus: ws.ntp.Status(),
	}
}

func (e *Engine) StopAll() {
	_ = e.Stop()
}

type workerState struct {
	cancel    context.CancelFunc
	done      chan struct{}
	spec      types.NetworkSpec
	freshness *freshness.Tracker
	ntp       *ntp.Checker
	ping      *ping.Tracker
	phase     WorkerPhase
	lastErr   string
	mu        sync.RWMutex
}

func (w *workerState) setStatus(phase WorkerPhase, lastErr string) {
	w.mu.Lock()
	if w.phase == 0 {
		w.phase = WorkerAbsent
	}
	w.phase = w.phase.Transition(phase)
	w.lastErr = lastErr
	w.mu.Unlock()
}

func (w *workerState) status() (WorkerPhase, string) {
	w.mu.RLock()
	defer w.mu.RUnlock()
	if w.phase == 0 {
		return WorkerAbsent, w.lastErr
	}
	return w.phase, w.lastErr
}

func (e *Engine) runWorkerLoop(ctx context.Context, ws *workerState, spec types.NetworkSpec) {
	networkName := defaults.NormalizeNetwork(spec.Network)
	log := slog.With("component", "runtime-engine", "network", networkName)
	ws.setStatus(WorkerStarting, "")

	var consecutiveFailures int
	retryDelay := workerRetryDelay

	// backoff logs the error, updates status, sleeps, and bumps the failure counter.
	// Returns false if the context was cancelled or max failures reached.
	backoff := func(msg string, err error) bool {
		log.Debug(msg, "err", err)
		ws.setStatus(WorkerBackoff, err.Error())
		if !sleepWithContext(ctx, retryDelay) {
			return false
		}
		consecutiveFailures++
		if consecutiveFailures >= workerMaxConsecutiveFailures {
			log.Error("worker giving up after too many consecutive failures", "failures", consecutiveFailures)
			ws.setStatus(WorkerGivingUp, fmt.Sprintf("gave up after %d consecutive failures: %v", consecutiveFailures, err))
			return false
		}
		retryDelay = min(retryDelay*2, workerMaxRetryDelay)
		return true
	}

	for {
		cfg, err := network.ConfigFromSpec(spec)
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
		ws.setStatus(WorkerRunning, "")
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
				ws.setStatus(WorkerDegraded, err.Error())
			},
		}

		err = worker.Run(ctx)
		_ = peerCtrl.Close()
		if ctx.Err() != nil {
			ws.setStatus(WorkerStopping, "")
			ws.setStatus(WorkerAbsent, "")
			log.Debug("worker loop canceled")
			return
		}
		if err != nil {
			log.Warn("worker loop exited with error", "err", err)
			ws.setStatus(WorkerBackoff, err.Error())
		} else {
			log.Debug("worker loop exited cleanly")
			ws.setStatus(WorkerBackoff, "")
		}

		if !sleepWithContext(ctx, retryDelay) {
			return
		}
	}
}

func (e *Engine) clockOrDefault() network.Clock {
	if e.clock != nil {
		return e.clock
	}
	return network.RealClock{}
}

func (e *Engine) loadSelfID(spec types.NetworkSpec) string {
	if e.stateStore == nil {
		return ""
	}
	cfg, err := network.ConfigFromSpec(spec)
	if err != nil {
		return ""
	}
	st, err := network.LoadState(e.stateStore, cfg)
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
