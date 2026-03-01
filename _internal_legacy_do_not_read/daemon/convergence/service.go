package convergence

import (
	"context"
	"fmt"
	"log/slog"
	"sync"
	"time"

	"ployz/internal/support/check"
	"ployz/internal/daemon/overlay"
	"ployz/pkg/sdk/defaults"
	"ployz/pkg/sdk/types"
)

const (
	// supervisorRetryDelay is 2s: short enough for quick recovery, long enough to avoid tight spin on persistent failures.
	supervisorRetryDelay = 2 * time.Second
	// supervisorMaxRetryDelay is 60s: caps exponential backoff to avoid excessive wait.
	supervisorMaxRetryDelay = 60 * time.Second
	// supervisorMaxConsecutiveFailures is 100: give up after sustained failure (~15 min at cap).
	supervisorMaxConsecutiveFailures = 100
	// stopNetworkTimeout is 30s: maximum time to wait for a supervisor goroutine to exit.
	stopNetworkTimeout = 30 * time.Second
)

type NetworkHealth struct {
	Peers     map[string]PeerHealth
	NTPStatus Status
}

type Engine struct {
	mu            sync.Mutex
	supervisor    *supervisorState
	rootCtx       context.Context
	newController NetworkControllerFactory
	newReconciler PeerReconcilerFactory
	newRegistry   RegistryFactory
	stateStore    overlay.StateStore
	clock         overlay.Clock
	pingDialFunc  func(context.Context, string) (time.Duration, error)
	ntpCheckFunc  func() Status
}

type Service = Engine
type Option = EngineOption

func NewService(ctx context.Context, opts ...Option) *Service {
	return New(ctx, opts...)
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

func WithStateStore(s overlay.StateStore) EngineOption {
	return func(e *Engine) { e.stateStore = s }
}

func WithClock(c overlay.Clock) EngineOption {
	return func(e *Engine) { e.clock = c }
}

func WithPingDialFunc(f func(context.Context, string) (time.Duration, error)) EngineOption {
	return func(e *Engine) { e.pingDialFunc = f }
}

func WithNTPCheckFunc(f func() Status) EngineOption {
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

	if existing := e.supervisor; existing != nil {
		log.Info("restarting supervisor loop")
		existing.cancel()
		<-existing.done
		e.supervisor = nil
	}

	supervisorCtx, cancel := context.WithCancel(e.rootCtx)

	selfID := e.loadSelfID(spec)

	clk := e.clockOrDefault()

	ss := &supervisorState{
		cancel:    cancel,
		done:      make(chan struct{}),
		spec:      spec,
		freshness: NewFreshnessTracker(selfID, clk),
		ntp:       NewChecker(clk),
		ping:      NewPingTracker(),
	}
	if e.ntpCheckFunc != nil {
		ss.ntp.CheckFunc = e.ntpCheckFunc
	}
	if e.pingDialFunc != nil {
		ss.ping.DialFunc = e.pingDialFunc
	}
	e.supervisor = ss
	log.Info("starting supervisor loop")

	go func() {
		defer close(ss.done)
		e.runSupervisorLoop(supervisorCtx, ss, spec)
	}()

	return nil
}

func (e *Engine) Stop() error {
	networkName := ""
	e.mu.Lock()
	if e.supervisor != nil {
		networkName = defaults.NormalizeNetwork(e.supervisor.spec.Network)
	}
	e.mu.Unlock()
	log := slog.With("component", "runtime-engine", "network", networkName)

	e.mu.Lock()
	ss := e.supervisor
	if ss == nil {
		e.mu.Unlock()
		return nil
	}
	ss.cancel()
	e.supervisor = nil
	e.mu.Unlock()

	select {
	case <-ss.done:
		log.Info("supervisor loop stopped")
	case <-time.After(stopNetworkTimeout):
		log.Warn("supervisor loop stop timed out")
	}
	return nil
}

func (e *Engine) Status() (phase SupervisorPhase, lastErr string) {
	e.mu.Lock()
	ss := e.supervisor
	e.mu.Unlock()

	if ss == nil {
		return SupervisorAbsent, ""
	}
	return ss.status()
}

func (e *Engine) Health() NetworkHealth {
	e.mu.Lock()
	ss := e.supervisor
	e.mu.Unlock()

	if ss == nil {
		return NetworkHealth{}
	}

	peers := ss.freshness.Snapshot()

	// Merge ping RTTs into peer health.
	pings := ss.ping.Snapshot()
	if len(pings) > 0 && peers == nil {
		peers = make(map[string]PeerHealth)
	}
	for nodeID, sample := range pings {
		ph := peers[nodeID]
		ph.PingPhase = sample.Phase
		ph.PingRTT = sample.RTT
		peers[nodeID] = ph
	}

	return NetworkHealth{
		Peers:     peers,
		NTPStatus: ss.ntp.Status(),
	}
}

func (e *Engine) StopAll() {
	_ = e.Stop()
}

type supervisorState struct {
	cancel    context.CancelFunc
	done      chan struct{}
	spec      types.NetworkSpec
	freshness *FreshnessTracker
	ntp       *Checker
	ping      *PingTracker
	phase     SupervisorPhase
	lastErr   string
	mu        sync.RWMutex
}

func (w *supervisorState) setStatus(phase SupervisorPhase, lastErr string) {
	w.mu.Lock()
	if w.phase == 0 {
		w.phase = SupervisorAbsent
	}
	w.phase = w.phase.Transition(phase)
	w.lastErr = lastErr
	w.mu.Unlock()
}

func (w *supervisorState) status() (SupervisorPhase, string) {
	w.mu.RLock()
	defer w.mu.RUnlock()
	if w.phase == 0 {
		return SupervisorAbsent, w.lastErr
	}
	return w.phase, w.lastErr
}

func (e *Engine) runSupervisorLoop(ctx context.Context, ss *supervisorState, spec types.NetworkSpec) {
	networkName := defaults.NormalizeNetwork(spec.Network)
	log := slog.With("component", "runtime-engine", "network", networkName)
	ss.setStatus(SupervisorStarting, "")

	var consecutiveFailures int
	retryDelay := supervisorRetryDelay

	// backoff logs the error, updates status, sleeps, and bumps the failure counter.
	// Returns false if the context was cancelled or max failures reached.
	backoff := func(msg string, err error) bool {
		log.Debug(msg, "err", err)
		ss.setStatus(SupervisorBackoff, err.Error())
		if !sleepWithContext(ctx, retryDelay) {
			return false
		}
		consecutiveFailures++
		if consecutiveFailures >= supervisorMaxConsecutiveFailures {
			log.Error("supervisor loop giving up after too many consecutive failures", "failures", consecutiveFailures)
			ss.setStatus(SupervisorGivingUp, fmt.Sprintf("gave up after %d consecutive failures: %v", consecutiveFailures, err))
			return false
		}
		retryDelay = min(retryDelay*2, supervisorMaxRetryDelay)
		return true
	}

	for {
		cfg, err := overlay.ConfigFromSpec(spec)
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
		retryDelay = supervisorRetryDelay
		ss.setStatus(SupervisorRunning, "")
		log.Debug("runtime prepared, entering supervisor loop")

		supervisorLoop := Supervisor{
			Spec:           runtimeCfg,
			Registry:       reg,
			PeerReconciler: peerCtrl,
			StateStore:     e.stateStore,
			Freshness:      ss.freshness,
			NTP:            ss.ntp,
			Ping:           ss.ping,
			OnFailure: func(err error) {
				if err == nil {
					return
				}
				ss.setStatus(SupervisorDegraded, err.Error())
			},
		}

		err = supervisorLoop.Run(ctx)
		_ = peerCtrl.Close()
		if ctx.Err() != nil {
			ss.setStatus(SupervisorStopping, "")
			ss.setStatus(SupervisorAbsent, "")
			log.Debug("supervisor loop canceled")
			return
		}
		if err != nil {
			log.Warn("supervisor loop exited with error", "err", err)
			ss.setStatus(SupervisorBackoff, err.Error())
		} else {
			log.Debug("supervisor loop exited cleanly")
			ss.setStatus(SupervisorBackoff, "")
		}

		if !sleepWithContext(ctx, retryDelay) {
			return
		}
	}
}

func (e *Engine) clockOrDefault() overlay.Clock {
	if e.clock != nil {
		return e.clock
	}
	return overlay.RealClock{}
}

func (e *Engine) loadSelfID(spec types.NetworkSpec) string {
	if e.stateStore == nil {
		return ""
	}
	cfg, err := overlay.ConfigFromSpec(spec)
	if err != nil {
		return ""
	}
	st, err := overlay.LoadState(e.stateStore, cfg)
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
