package engine

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net/netip"
	"strings"
	"sync"
	"time"

	pb "ployz/internal/daemon/pb"
	netctrl "ployz/internal/network"
	"ployz/internal/reconcile"
	"ployz/pkg/sdk/defaults"
)

type workerState struct {
	cancel    context.CancelFunc
	done      chan struct{}
	spec      *pb.NetworkSpec
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

type NetworkHealth struct {
	Peers     map[string]reconcile.PeerHealth
	NTPStatus reconcile.NTPStatus
}

type Engine struct {
	mu              sync.Mutex
	workers         map[string]*workerState
	rootCtx         context.Context
	newController   NetworkControllerFactory   // creates controllers for Start
	newReconciler   PeerReconcilerFactory      // creates peer reconcilers for workers
	newRegistry     RegistryFactory            // creates registries for workers
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

func New(ctx context.Context, opts ...EngineOption) *Engine {
	e := &Engine{
		workers: make(map[string]*workerState),
		rootCtx: ctx,
	}
	for _, opt := range opts {
		opt(e)
	}
	return e
}

func (e *Engine) StartNetwork(ctx context.Context, spec *pb.NetworkSpec) error {
	network := defaults.NormalizeNetwork(spec.Network)
	if network == "" {
		return fmt.Errorf("network is required")
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

	// Determine self ID for freshness tracker.
	selfID := ""
	if cfg, err := configFromSpec(spec); err == nil {
		if st, loadErr := netctrl.LoadState(cfg); loadErr == nil {
			selfID = st.WGPublic
		}
	}

	ft := reconcile.NewFreshnessTracker(selfID)
	ntpChecker := reconcile.NewNTPChecker()
	pingTracker := reconcile.NewPingTracker()

	ws := &workerState{
		cancel:    cancel,
		done:      make(chan struct{}),
		spec:      spec,
		freshness: ft,
		ntp:       ntpChecker,
		ping:      pingTracker,
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

	<-ws.done
	log.Info("worker stopped")
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

	var peers map[string]reconcile.PeerHealth
	if ws.freshness != nil {
		peers = ws.freshness.Snapshot()
	}

	// Merge ping RTTs into peer health.
	if ws.ping != nil {
		pings := ws.ping.Snapshot()
		if peers == nil {
			peers = make(map[string]reconcile.PeerHealth)
		}
		for nodeID, rtt := range pings {
			ph := peers[nodeID]
			ph.PingRTT = rtt
			peers[nodeID] = ph
		}
	}

	var ntpStatus reconcile.NTPStatus
	if ws.ntp != nil {
		ntpStatus = ws.ntp.Status()
	}

	return NetworkHealth{
		Peers:     peers,
		NTPStatus: ntpStatus,
	}
}

func (e *Engine) StopAll() {
	log := slog.With("component", "runtime-engine")
	e.mu.Lock()
	workers := make(map[string]*workerState, len(e.workers))
	for k, v := range e.workers {
		workers[k] = v
	}
	e.mu.Unlock()

	if len(workers) > 0 {
		log.Info("stopping all workers", "count", len(workers))
	}
	for network, ws := range workers {
		ws.cancel()
		<-ws.done
		e.mu.Lock()
		delete(e.workers, network)
		e.mu.Unlock()
		log.Debug("worker stopped", "network", network)
	}
}

func (e *Engine) runWorkerLoop(ctx context.Context, ws *workerState, spec *pb.NetworkSpec) {
	network := defaults.NormalizeNetwork(spec.Network)
	log := slog.With("component", "runtime-engine", "network", network)
	for {
		cfg, err := configFromSpec(spec)
		if err != nil {
			log.Debug("invalid network spec", "err", err)
			ws.setStatus(false, err.Error())
			if !sleepWithContext(ctx, 2*time.Second) {
				return
			}
			continue
		}

		startCtrl, err := e.newController()
		if err != nil {
			log.Debug("create controller failed", "err", err)
			ws.setStatus(false, err.Error())
			if !sleepWithContext(ctx, 2*time.Second) {
				return
			}
			continue
		}
		runtimeCfg, startErr := startCtrl.Start(ctx, cfg)
		_ = startCtrl.Close()
		if startErr != nil {
			log.Debug("start runtime failed", "err", startErr)
			ws.setStatus(false, startErr.Error())
			if !sleepWithContext(ctx, 2*time.Second) {
				return
			}
			continue
		}

		peerCtrl, err := e.newReconciler()
		if err != nil {
			log.Debug("create peer reconciler failed", "err", err)
			ws.setStatus(false, err.Error())
			if !sleepWithContext(ctx, 2*time.Second) {
				return
			}
			continue
		}

		reg := e.newRegistry(runtimeCfg.CorrosionAPIAddr, runtimeCfg.CorrosionAPIToken)

		ws.setStatus(true, "")
		log.Debug("runtime prepared, entering reconcile loop")

		worker := reconcile.Worker{
			Spec:           runtimeCfg,
			Registry:       reg,
			PeerReconciler: peerCtrl,
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
		if errors.Is(err, context.Canceled) || ctx.Err() != nil {
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

		if !sleepWithContext(ctx, 2*time.Second) {
			return
		}
	}
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

func configFromSpec(spec *pb.NetworkSpec) (netctrl.Config, error) {
	cfg := netctrl.Config{
		Network:           defaults.NormalizeNetwork(spec.Network),
		DataRoot:          strings.TrimSpace(spec.DataRoot),
		AdvertiseEP:       strings.TrimSpace(spec.AdvertiseEndpoint),
		WGPort:            int(spec.WgPort),
		CorrosionMemberID: spec.CorrosionMemberId,
		CorrosionAPIToken: strings.TrimSpace(spec.CorrosionApiToken),
		HelperImage:       strings.TrimSpace(spec.HelperImage),
	}
	for _, bs := range spec.Bootstrap {
		bs = netctrl.NormalizeBootstrapAddrPort(bs)
		if bs == "" {
			continue
		}
		cfg.CorrosionBootstrap = append(cfg.CorrosionBootstrap, bs)
	}

	if strings.TrimSpace(spec.NetworkCidr) != "" {
		pfx, err := netip.ParsePrefix(strings.TrimSpace(spec.NetworkCidr))
		if err != nil {
			return netctrl.Config{}, fmt.Errorf("parse network cidr: %w", err)
		}
		cfg.NetworkCIDR = pfx
	}
	if strings.TrimSpace(spec.Subnet) != "" {
		pfx, err := netip.ParsePrefix(strings.TrimSpace(spec.Subnet))
		if err != nil {
			return netctrl.Config{}, fmt.Errorf("parse subnet: %w", err)
		}
		cfg.Subnet = pfx
	}
	if strings.TrimSpace(spec.ManagementIp) != "" {
		addr, err := netip.ParseAddr(strings.TrimSpace(spec.ManagementIp))
		if err != nil {
			return netctrl.Config{}, fmt.Errorf("parse management ip: %w", err)
		}
		cfg.Management = addr
	}

	return netctrl.NormalizeConfig(cfg)
}
