package engine

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/netip"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"ployz/internal/control/state"
	pb "ployz/internal/daemon/pb"
	netctrl "ployz/internal/machine/network"
	"ployz/internal/runtime/reconcile"
	"ployz/pkg/sdk/defaults"
)

type workerHandle struct {
	cancel    context.CancelFunc
	done      chan struct{}
	spec      *pb.NetworkSpec
	signature string
}

type Engine struct {
	ctx      context.Context
	dataRoot string
	store    *state.Store

	mu      sync.Mutex
	workers map[string]*workerHandle
}

func Run(ctx context.Context, dataRoot string) error {
	if strings.TrimSpace(dataRoot) == "" {
		dataRoot = defaults.DataRoot()
	}
	store, err := state.Open(filepath.Join(dataRoot, "daemon.db"))
	if err != nil {
		return err
	}
	defer store.Close()

	e := &Engine{
		ctx:      ctx,
		dataRoot: dataRoot,
		store:    store,
		workers:  make(map[string]*workerHandle),
	}
	if err := e.syncOnce(); err != nil {
		return err
	}

	ticker := time.NewTicker(2 * time.Second)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			e.stopAllWorkers()
			return nil
		case <-ticker.C:
			if err := e.syncOnce(); err != nil {
				continue
			}
		}
	}
}

func (e *Engine) syncOnce() error {
	persisted, err := e.store.ListSpecs()
	if err != nil {
		return err
	}

	enabled := make(map[string]state.PersistedSpec, len(persisted))
	for _, item := range persisted {
		network := defaults.NormalizeNetwork(item.Spec.Network)
		if !item.Enabled || network == "" {
			continue
		}
		item.Spec.Network = network
		enabled[network] = item
	}

	e.mu.Lock()
	defer e.mu.Unlock()

	for network, handle := range e.workers {
		item, ok := enabled[network]
		if !ok {
			e.stopWorkerLocked(network, handle)
			continue
		}
		sig := specSignature(item.Spec)
		if sig == handle.signature {
			continue
		}
		e.stopWorkerLocked(network, handle)
		e.startWorkerLocked(item.Spec, sig)
	}

	for network, item := range enabled {
		if _, ok := e.workers[network]; ok {
			continue
		}
		e.startWorkerLocked(item.Spec, specSignature(item.Spec))
	}

	return nil
}

func (e *Engine) startWorkerLocked(spec *pb.NetworkSpec, signature string) {
	network := defaults.NormalizeNetwork(spec.Network)
	if network == "" {
		return
	}

	ctx, cancel := context.WithCancel(e.ctx)
	h := &workerHandle{cancel: cancel, done: make(chan struct{}), spec: spec, signature: signature}
	e.workers[network] = h

	go func() {
		defer close(h.done)
		e.runWorkerLoop(ctx, spec)
	}()
}

func (e *Engine) runWorkerLoop(ctx context.Context, spec *pb.NetworkSpec) {
	network := defaults.NormalizeNetwork(spec.Network)
	for {
		cfg, err := configFromSpec(spec)
		if err != nil {
			_ = e.store.SetRuntimeStatus(network, false, err.Error())
			if !sleepWithContext(ctx, 2*time.Second) {
				return
			}
			continue
		}

		ctrl, err := netctrl.New()
		if err != nil {
			_ = e.store.SetRuntimeStatus(network, false, err.Error())
			if !sleepWithContext(ctx, 2*time.Second) {
				return
			}
			continue
		}
		_, startErr := ctrl.Start(ctx, cfg)
		_ = ctrl.Close()
		if startErr != nil {
			_ = e.store.SetRuntimeStatus(network, false, startErr.Error())
			if !sleepWithContext(ctx, 2*time.Second) {
				return
			}
			continue
		}

		_ = e.store.SetRuntimeStatus(network, true, "")

		worker := reconcile.Worker{
			Spec: cfg,
			OnFailure: func(err error) {
				if err == nil {
					return
				}
				_ = e.store.SetRuntimeStatus(network, true, err.Error())
			},
		}

		err = worker.Run(ctx)
		if errors.Is(err, context.Canceled) || ctx.Err() != nil {
			_ = e.store.SetRuntimeStatus(network, false, "")
			return
		}
		if err != nil {
			_ = e.store.SetRuntimeStatus(network, false, err.Error())
		} else {
			_ = e.store.SetRuntimeStatus(network, false, "")
		}

		if !sleepWithContext(ctx, 2*time.Second) {
			return
		}
	}
}

func (e *Engine) stopWorkerLocked(network string, handle *workerHandle) {
	handle.cancel()
	<-handle.done
	delete(e.workers, network)
	_ = e.store.SetRuntimeStatus(network, false, "")
}

func (e *Engine) stopAllWorkers() {
	e.mu.Lock()
	networks := make([]string, 0, len(e.workers))
	for network := range e.workers {
		networks = append(networks, network)
	}
	e.mu.Unlock()

	for _, network := range networks {
		e.mu.Lock()
		h := e.workers[network]
		if h != nil {
			e.stopWorkerLocked(network, h)
		}
		e.mu.Unlock()
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

func specSignature(spec *pb.NetworkSpec) string {
	payload, err := json.Marshal(spec)
	if err != nil {
		return ""
	}
	return string(payload)
}

func configFromSpec(spec *pb.NetworkSpec) (netctrl.Config, error) {
	cfg := netctrl.Config{
		Network:     defaults.NormalizeNetwork(spec.Network),
		DataRoot:    strings.TrimSpace(spec.DataRoot),
		AdvertiseEP: strings.TrimSpace(spec.AdvertiseEndpoint),
		WGPort:      int(spec.WgPort),
		HelperImage: strings.TrimSpace(spec.HelperImage),
	}
	for _, bs := range spec.Bootstrap {
		bs = strings.TrimSpace(bs)
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
