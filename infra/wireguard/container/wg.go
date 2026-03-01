package container

import (
	"context"
	"fmt"
	"log/slog"
	"net/netip"
	"strings"
	"sync"
	"time"

	"ployz"
	"ployz/infra/docker"
	"ployz/infra/wireguard"

	"github.com/containerd/errdefs"
	"github.com/docker/docker/api/types/container"
	"github.com/docker/docker/api/types/mount"
	"github.com/docker/docker/api/types/network"
	"github.com/docker/docker/client"
	"github.com/docker/go-connections/nat"
	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

const peerKeepalive = 25 * time.Second

// Config holds the static configuration for a containerized WireGuard device.
type Config struct {
	Interface     string
	MTU           int
	PrivateKey    wgtypes.Key
	Port          int
	MgmtIP        netip.Addr
	Image         string
	ContainerName string
	NetworkName   string
	// HostPort publishes the WG UDP port to the host when non-zero.
	// Needed on macOS for the overlay bridge and inbound mesh peering.
	HostPort int
}

// WG implements mesh.WireGuard by running WireGuard inside a Docker
// container. The container uses the kernel WireGuard module available
// in the Docker VM (OrbStack on macOS).
type WG struct {
	cfg    Config
	docker client.APIClient

	mu         sync.Mutex
	peerOwners map[string]wireguard.PeerOwner // pubkey -> owner
}

// New creates a containerized WireGuard implementation.
func New(cfg Config, docker client.APIClient) *WG {
	return &WG{
		cfg:        cfg,
		docker:     docker,
		peerOwners: make(map[string]wireguard.PeerOwner),
	}
}

// Up ensures the Docker network and WireGuard container exist, then
// creates and configures the WireGuard interface inside the container.
// Idempotent — reuses a running container if one exists.
func (w *WG) Up(ctx context.Context) error {
	w.mu.Lock()
	defer w.mu.Unlock()

	if err := ensureNetwork(ctx, w.docker, w.cfg.NetworkName); err != nil {
		return fmt.Errorf("ensure mesh network: %w", err)
	}

	if err := w.ensureContainer(ctx); err != nil {
		return err
	}

	if err := w.configureInterface(ctx); err != nil {
		return fmt.Errorf("configure wireguard interface: %w", err)
	}

	return nil
}

// SetPeers replaces the current peer set with the desired state.
func (w *WG) SetPeers(ctx context.Context, peers []ployz.MachineRecord) error {
	w.mu.Lock()
	defer w.mu.Unlock()

	// Get current peers to detect stale ones.
	currentPeers, err := w.listPeers(ctx)
	if err != nil {
		return fmt.Errorf("list current peers: %w", err)
	}

	if w.peerOwners == nil {
		w.peerOwners = make(map[string]wireguard.PeerOwner)
	}

	desired := make(map[string]struct{}, len(peers))
	for _, p := range peers {
		key := p.PublicKey.String()
		desired[key] = struct{}{}
		w.peerOwners[key] = wireguard.PeerOwnerMesh

		args := []string{
			"wg", "set", w.cfg.Interface,
			"peer", p.PublicKey.String(),
			"persistent-keepalive", fmt.Sprintf("%d", int(peerKeepalive.Seconds())),
		}

		if p.OverlayIP.IsValid() {
			prefix := wireguard.HostPrefix(p.OverlayIP)
			args = append(args, "allowed-ips", prefix.String())
		}

		if len(p.Endpoints) > 0 {
			args = append(args, "endpoint", p.Endpoints[0].String())
		}

		if _, err := exec(ctx, w.docker, w.cfg.ContainerName, args...); err != nil {
			return fmt.Errorf("set peer %s: %w", p.PublicKey, err)
		}
	}

	// Remove stale mesh peers. Non-mesh peers (bridge, session) are
	// managed by their respective owners and never removed here.
	for _, key := range currentPeers {
		if _, ok := desired[key]; ok {
			continue
		}
		if owner := w.peerOwners[key]; owner != "" && owner != wireguard.PeerOwnerMesh {
			continue
		}
		args := []string{"wg", "set", w.cfg.Interface, "peer", key, "remove"}
		if _, err := exec(ctx, w.docker, w.cfg.ContainerName, args...); err != nil {
			return fmt.Errorf("remove stale peer %s: %w", key, err)
		}
		delete(w.peerOwners, key)
	}

	// Sync routes for peer overlay IPs.
	if err := w.syncRoutes(ctx, peers); err != nil {
		return fmt.Errorf("sync routes: %w", err)
	}

	return nil
}

// AddPeer registers a peer with the given owner. Peers added via AddPeer
// are never removed by SetPeers. Must be called after Up().
func (w *WG) AddPeer(ctx context.Context, owner wireguard.PeerOwner, pubKey wgtypes.Key, allowedIP netip.Addr) error {
	w.mu.Lock()
	defer w.mu.Unlock()

w.peerOwners[pubKey.String()] = owner

	prefix := wireguard.HostPrefix(allowedIP)
	args := []string{
		"wg", "set", w.cfg.Interface,
		"peer", pubKey.String(),
		"persistent-keepalive", fmt.Sprintf("%d", int(peerKeepalive.Seconds())),
		"allowed-ips", prefix.String(),
	}
	if _, err := exec(ctx, w.docker, w.cfg.ContainerName, args...); err != nil {
		return fmt.Errorf("add %s peer %s: %w", owner, pubKey, err)
	}

	if _, err := exec(ctx, w.docker, w.cfg.ContainerName,
		"ip", "route", "replace", prefix.String(), "dev", w.cfg.Interface,
	); err != nil {
		return fmt.Errorf("add %s peer route %s: %w", owner, prefix, err)
	}
	return nil
}

// Down stops and removes the WireGuard container. Idempotent.
func (w *WG) Down(ctx context.Context) error {
	w.mu.Lock()
	defer w.mu.Unlock()

	return docker.StopAndRemove(ctx, w.docker, w.cfg.ContainerName)
}

// ensureContainer inspects, starts, or creates the WireGuard container.
// If a HostPort is configured and an existing container lacks the published
// port binding, the container is recreated.
func (w *WG) ensureContainer(ctx context.Context) error {
	info, err := w.docker.ContainerInspect(ctx, w.cfg.ContainerName)
	if err == nil {
		if w.cfg.HostPort != 0 && !w.hasPublishedPort(info) {
			slog.Info("Recreating WireGuard container for port publishing.", "name", w.cfg.ContainerName)
			if err := w.removeContainer(ctx); err != nil {
				return fmt.Errorf("recreate wireguard container: %w", err)
			}
		} else {
			if info.State.Running {
				slog.Info("Reusing running WireGuard container.", "name", w.cfg.ContainerName)
				return nil
			}
			if err := w.docker.ContainerStart(ctx, w.cfg.ContainerName, container.StartOptions{}); err != nil {
				return fmt.Errorf("start existing wireguard container: %w", err)
			}
			slog.Info("Started existing WireGuard container.", "name", w.cfg.ContainerName)
			return nil
		}
	} else if !errdefs.IsNotFound(err) {
		return fmt.Errorf("inspect wireguard container: %w", err)
	}

	if err := w.createAndStart(ctx); err != nil {
		return fmt.Errorf("create wireguard container: %w", err)
	}

	slog.Info("WireGuard container started.", "name", w.cfg.ContainerName)
	return nil
}

// removeContainer stops a container, waits for it to exit, then removes it.
// Waiting ensures the host port binding is fully released before we try to
// create a replacement container.
func (w *WG) removeContainer(ctx context.Context) error {
	_ = w.docker.ContainerStop(ctx, w.cfg.ContainerName, container.StopOptions{})

	// Wait for the container to reach a non-running state so the kernel
	// releases the port binding before we attempt to rebind it.
	waitCh, errCh := w.docker.ContainerWait(ctx, w.cfg.ContainerName, container.WaitConditionNotRunning)
	select {
	case <-waitCh:
	case err := <-errCh:
		// NotFound means it's already gone — that's fine.
		if err != nil && !errdefs.IsNotFound(err) {
			slog.Warn("Error waiting for container stop.", "err", err)
		}
	case <-ctx.Done():
		return ctx.Err()
	}

	if err := w.docker.ContainerRemove(ctx, w.cfg.ContainerName, container.RemoveOptions{Force: true}); err != nil {
		if !errdefs.IsNotFound(err) {
			return fmt.Errorf("remove container: %w", err)
		}
	}
	return nil
}

// hasPublishedPort checks whether a container has the expected UDP port published.
func (w *WG) hasPublishedPort(info container.InspectResponse) bool {
	portKey := nat.Port(fmt.Sprintf("%d/udp", w.cfg.Port))
	bindings, ok := info.HostConfig.PortBindings[portKey]
	return ok && len(bindings) > 0
}

func (w *WG) createAndStart(ctx context.Context) error {
	containerCfg := &container.Config{
		Image: w.cfg.Image,
		Cmd:   []string{"sleep", "infinity"},
	}

	hostCfg := &container.HostConfig{
		RestartPolicy: container.RestartPolicy{
			Name: container.RestartPolicyAlways,
		},
		CapAdd: []string{"NET_ADMIN"},
		Mounts: []mount.Mount{
			{
				Type:   mount.TypeBind,
				Source: "/dev/net/tun",
				Target: "/dev/net/tun",
			},
		},
		Sysctls: map[string]string{
			"net.ipv4.conf.all.src_valid_mark": "1",
		},
	}

	if w.cfg.HostPort != 0 {
		containerPort := nat.Port(fmt.Sprintf("%d/udp", w.cfg.Port))
		containerCfg.ExposedPorts = nat.PortSet{containerPort: struct{}{}}
		hostCfg.PortBindings = nat.PortMap{
			containerPort: []nat.PortBinding{{
				HostPort: fmt.Sprintf("%d", w.cfg.HostPort),
			}},
		}
	}

	networkCfg := &network.NetworkingConfig{
		EndpointsConfig: map[string]*network.EndpointSettings{
			w.cfg.NetworkName: {},
		},
	}

	return docker.CreateAndStart(ctx, w.docker, w.cfg.ContainerName, w.cfg.Image, containerCfg, hostCfg, networkCfg)
}

// configureInterface creates and configures the WireGuard interface
// inside the container using ip and wg commands.
func (w *WG) configureInterface(ctx context.Context) error {
	name := w.cfg.ContainerName
	iface := w.cfg.Interface
	// Create WireGuard interface.
	if _, err := exec(ctx, w.docker, name, "ip", "link", "add", iface, "type", "wireguard"); err != nil {
		// Interface may already exist from a previous Up.
		slog.Debug("WireGuard interface may already exist.", "err", err)
	}

	// Set MTU.
	if _, err := exec(ctx, w.docker, name,
		"ip", "link", "set", iface, "mtu", fmt.Sprintf("%d", w.cfg.MTU),
	); err != nil {
		return fmt.Errorf("set mtu: %w", err)
	}

	// Configure WireGuard private key and listen port.
	// wg set reads the key from a file in base64 format.
	if err := w.configureKeyViaFile(ctx); err != nil {
		return err
	}

	// Assign management IP address.
	if w.cfg.MgmtIP.IsValid() {
		prefix := wireguard.HostPrefix(w.cfg.MgmtIP)
		if _, err := exec(ctx, w.docker, name,
			"ip", "addr", "add", prefix.String(), "dev", iface,
		); err != nil {
			slog.Debug("Address may already exist.", "addr", prefix, "err", err)
		}
	}

	// Bring interface up.
	if _, err := exec(ctx, w.docker, name,
		"ip", "link", "set", iface, "up",
	); err != nil {
		return fmt.Errorf("bring interface up: %w", err)
	}

	return nil
}

// configureKeyViaFile writes the private key to a temp file inside
// the container and configures WireGuard from it.
func (w *WG) configureKeyViaFile(ctx context.Context) error {
	name := w.cfg.ContainerName
	iface := w.cfg.Interface
	keyPath := "/tmp/wg-private-key"
	keyBase64 := w.cfg.PrivateKey.String()

	// Write key to file. wg expects base64 encoding.
	if _, err := exec(ctx, w.docker, name,
		"sh", "-c", fmt.Sprintf("echo '%s' > %s && chmod 600 %s", keyBase64, keyPath, keyPath),
	); err != nil {
		return fmt.Errorf("write private key file: %w", err)
	}

	// Configure WireGuard.
	if _, err := exec(ctx, w.docker, name,
		"wg", "set", iface,
		"private-key", keyPath,
		"listen-port", fmt.Sprintf("%d", w.cfg.Port),
	); err != nil {
		return fmt.Errorf("configure wireguard: %w", err)
	}

	// Remove key file.
	if _, err := exec(ctx, w.docker, name, "rm", "-f", keyPath); err != nil {
		slog.Warn("Failed to remove private key file.", "err", err)
	}

	return nil
}

// listPeers returns the public keys of all currently configured peers.
func (w *WG) listPeers(ctx context.Context) ([]string, error) {
	out, err := exec(ctx, w.docker, w.cfg.ContainerName,
		"wg", "show", w.cfg.Interface, "peers",
	)
	if err != nil {
		return nil, err
	}

	var peers []string
	for _, line := range splitLines(out) {
		if line != "" {
			peers = append(peers, line)
		}
	}
	return peers, nil
}

// syncRoutes ensures routes exist inside the container for each peer's
// overlay IP, pointing at the WireGuard interface.
func (w *WG) syncRoutes(ctx context.Context, peers []ployz.MachineRecord) error {
	name := w.cfg.ContainerName
	iface := w.cfg.Interface

	for _, p := range peers {
		if !p.OverlayIP.IsValid() {
			continue
		}
		prefix := wireguard.HostPrefix(p.OverlayIP)
		if _, err := exec(ctx, w.docker, name,
			"ip", "route", "replace", prefix.String(), "dev", iface,
		); err != nil {
			return fmt.Errorf("add route %s: %w", prefix, err)
		}
	}
	return nil
}

// splitLines splits output bytes into lines.
func splitLines(data []byte) []string {
	return strings.Split(string(data), "\n")
}

// Verify WG satisfies the interface at compile time.
var _ interface {
	Up(context.Context) error
	SetPeers(context.Context, []ployz.MachineRecord) error
	Down(context.Context) error
} = (*WG)(nil)
