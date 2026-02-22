package proxy

import (
	"context"
	"fmt"
	"log/slog"
	"sync"

	"github.com/siderolabs/grpc-proxy/proxy"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/metadata"
	"google.golang.org/grpc/status"
)

// Director manages routing of gRPC requests between local and remote backends.
type Director struct {
	localBackend   *LocalBackend
	remotePort     uint16
	remoteBackends sync.Map
	mapper         MachineMapper

	mu           sync.RWMutex
	localAddress string
	localID      string
}

func NewDirector(localSockPath string, remotePort uint16, mapper MachineMapper) *Director {
	return &Director{
		localBackend: NewLocalBackend(localSockPath, "", ""),
		remotePort:   remotePort,
		mapper:       mapper,
	}
}

// UpdateLocalMachine updates the local machine details used to identify which
// requests should be proxied to the local gRPC server.
func (d *Director) UpdateLocalMachine(id, addr string) {
	d.mu.Lock()
	defer d.mu.Unlock()

	d.localAddress = addr
	d.localID = id
	d.localBackend = NewLocalBackend(d.localBackend.sockPath, addr, id)
	slog.Info("proxy local machine updated", "component", "proxy-director", "machine_id", id, "management_ip", addr)
}

// Director implements proxy.StreamDirector for grpc-proxy.
func (d *Director) Director(ctx context.Context, fullMethodName string) (proxy.Mode, []proxy.Backend, error) {
	md, ok := metadata.FromIncomingContext(ctx)
	if !ok {
		return proxy.One2One, []proxy.Backend{d.localBackend}, nil
	}
	// If the request is already proxied, send it to the local backend.
	if _, ok = md["proxy-authority"]; ok {
		return proxy.One2One, []proxy.Backend{d.localBackend}, nil
	}
	// If no machines metadata, send to local backend.
	machineIDs, ok := md["machines"]
	if !ok {
		return proxy.One2One, []proxy.Backend{d.localBackend}, nil
	}
	if len(machineIDs) == 0 {
		return proxy.One2One, nil, status.Error(codes.InvalidArgument, "no machines specified")
	}

	// Get the network from metadata.
	network := ""
	if nets := md["proxy-network"]; len(nets) > 0 {
		network = nets[0]
	}

	// Resolve machines.
	targets, err := resolveMachines(ctx, d.mapper, network, machineIDs)
	if err != nil {
		return proxy.One2One, nil, status.Error(codes.InvalidArgument, fmt.Sprintf("resolve machines: %v", err))
	}

	d.mu.RLock()
	localAddress := d.localAddress
	localBackend := d.localBackend
	d.mu.RUnlock()

	backends := make([]proxy.Backend, len(targets))
	for i, t := range targets {
		if t.ManagementIP == localAddress {
			backends[i] = localBackend
			continue
		}

		// Prefer overlay IPv4 for dialing — it works through the host overlay
		// on macOS where management IPv6 return traffic can't be routed.
		dialAddr := t.ManagementIP
		if t.OverlayIP != "" {
			dialAddr = t.OverlayIP
		}

		backend, err := d.remoteBackend(dialAddr, t.ID)
		if err != nil {
			return proxy.One2One, nil, status.Error(codes.Internal, err.Error())
		}
		backends[i] = backend
	}

	// Always use One2Many for consistent metadata injection.
	return proxy.One2Many, backends, nil
}

func (d *Director) remoteBackend(addr, id string) (*RemoteBackend, error) {
	b, ok := d.remoteBackends.Load(addr)
	if ok {
		return b.(*RemoteBackend), nil
	}

	backend, err := NewRemoteBackend(addr, d.remotePort, id)
	if err != nil {
		return nil, err
	}
	existing, loaded := d.remoteBackends.LoadOrStore(addr, backend)
	if loaded {
		backend.Close()
		return existing.(*RemoteBackend), nil
	}
	slog.Debug("proxy remote backend created", "component", "proxy-director", "machine_id", id, "management_ip", addr)
	return backend, nil
}

// FlushRemoteBackends closes all remote backend connections.
func (d *Director) FlushRemoteBackends() {
	closed := 0
	d.remoteBackends.Range(func(key, value interface{}) bool {
		backend, ok := value.(*RemoteBackend)
		if !ok {
			return true
		}
		backend.Close()
		d.remoteBackends.Delete(key)
		closed++
		return true
	})
	if closed > 0 {
		slog.Debug("proxy remote backends flushed", "component", "proxy-director", "count", closed)
	}
}

// Close closes all backend connections.
func (d *Director) Close() {
	d.localBackend.Close()
	d.FlushRemoteBackends()
}
