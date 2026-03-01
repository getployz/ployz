package container

import (
	"context"
	"encoding/hex"
	"fmt"
	"io"
	"log/slog"
	"net/netip"
	"sync"
	"time"

	"ployz"
	"ployz/infra/wireguard"

	"github.com/containerd/errdefs"
	"github.com/docker/docker/api/types/container"
	"github.com/docker/docker/api/types/image"
	"github.com/docker/docker/api/types/mount"
	"github.com/docker/docker/api/types/network"
	"github.com/docker/docker/client"
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
}

// WG implements mesh.WireGuard by running WireGuard inside a Docker
// container. The container uses the kernel WireGuard module available
// in the Docker VM (OrbStack on macOS).
type WG struct {
	cfg    Config
	docker client.APIClient

	mu sync.Mutex
}

// New creates a containerized WireGuard implementation.
func New(cfg Config, docker client.APIClient) *WG {
	return &WG{cfg: cfg, docker: docker}
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

	desired := make(map[string]struct{}, len(peers))
	for _, p := range peers {
		desired[p.PublicKey.String()] = struct{}{}

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

	// Remove stale peers.
	for _, key := range currentPeers {
		if _, ok := desired[key]; ok {
			continue
		}
		args := []string{"wg", "set", w.cfg.Interface, "peer", key, "remove"}
		if _, err := exec(ctx, w.docker, w.cfg.ContainerName, args...); err != nil {
			return fmt.Errorf("remove stale peer %s: %w", key, err)
		}
	}

	// Sync routes for peer overlay IPs.
	if err := w.syncRoutes(ctx, peers); err != nil {
		return fmt.Errorf("sync routes: %w", err)
	}

	return nil
}

// Down stops and removes the WireGuard container. Idempotent.
func (w *WG) Down(ctx context.Context) error {
	w.mu.Lock()
	defer w.mu.Unlock()

	if err := w.docker.ContainerStop(ctx, w.cfg.ContainerName, container.StopOptions{}); err != nil {
		if !errdefs.IsNotFound(err) {
			return fmt.Errorf("stop wireguard container: %w", err)
		}
	}
	if err := w.docker.ContainerRemove(ctx, w.cfg.ContainerName, container.RemoveOptions{Force: true}); err != nil {
		if !errdefs.IsNotFound(err) {
			return fmt.Errorf("remove wireguard container: %w", err)
		}
	}
	return nil
}

// ensureContainer inspects, starts, or creates the WireGuard container.
func (w *WG) ensureContainer(ctx context.Context) error {
	info, err := w.docker.ContainerInspect(ctx, w.cfg.ContainerName)
	if err == nil {
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

	if !errdefs.IsNotFound(err) {
		return fmt.Errorf("inspect wireguard container: %w", err)
	}

	if err := w.createAndStart(ctx); err != nil {
		return fmt.Errorf("create wireguard container: %w", err)
	}

	slog.Info("WireGuard container started.", "name", w.cfg.ContainerName)
	return nil
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

	networkCfg := &network.NetworkingConfig{
		EndpointsConfig: map[string]*network.EndpointSettings{
			w.cfg.NetworkName: {},
		},
	}

	_, err := w.docker.ContainerCreate(ctx, containerCfg, hostCfg, networkCfg, nil, w.cfg.ContainerName)
	if err != nil {
		if !errdefs.IsNotFound(err) {
			return fmt.Errorf("create container: %w", err)
		}
		if err := w.pullImage(ctx); err != nil {
			return err
		}
		if _, err = w.docker.ContainerCreate(ctx, containerCfg, hostCfg, networkCfg, nil, w.cfg.ContainerName); err != nil {
			return fmt.Errorf("create container after pull: %w", err)
		}
	}

	if err := w.docker.ContainerStart(ctx, w.cfg.ContainerName, container.StartOptions{}); err != nil {
		return fmt.Errorf("start container: %w", err)
	}
	return nil
}

func (w *WG) pullImage(ctx context.Context) error {
	slog.Info("Pulling WireGuard image.", "image", w.cfg.Image)
	resp, err := w.docker.ImagePull(ctx, w.cfg.Image, image.PullOptions{})
	if err != nil {
		return fmt.Errorf("pull wireguard image: %w", err)
	}
	defer resp.Close()
	if _, err := io.Copy(io.Discard, resp); err != nil {
		return fmt.Errorf("pull wireguard image: read response: %w", err)
	}
	return nil
}

// configureInterface creates and configures the WireGuard interface
// inside the container using ip and wg commands.
func (w *WG) configureInterface(ctx context.Context) error {
	name := w.cfg.ContainerName
	iface := w.cfg.Interface
	keyHex := hex.EncodeToString(w.cfg.PrivateKey[:])

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

	// Configure WireGuard private key and listen port via stdin pipe.
	// wg set requires the key in hex on the command line or via a file.
	// We use wg setconf with a generated config for atomicity but
	// for initial setup, wg set is simpler.
	if _, err := exec(ctx, w.docker, name,
		"wg", "set", iface,
		"private-key", "/dev/stdin",
		"listen-port", fmt.Sprintf("%d", w.cfg.Port),
	); err != nil {
		// wg set can't read from /dev/stdin via docker exec easily.
		// Fall back to writing a temp file.
		if err := w.configureKeyViaFile(ctx, keyHex); err != nil {
			return err
		}
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
func (w *WG) configureKeyViaFile(ctx context.Context, keyHex string) error {
	name := w.cfg.ContainerName
	iface := w.cfg.Interface
	keyPath := "/tmp/wg-private-key"

	// Write key to file.
	if _, err := exec(ctx, w.docker, name,
		"sh", "-c", fmt.Sprintf("echo '%s' > %s && chmod 600 %s", keyHex, keyPath, keyPath),
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

// splitLines splits output bytes into trimmed non-empty lines.
func splitLines(data []byte) []string {
	var lines []string
	start := 0
	for i, b := range data {
		if b == '\n' {
			line := string(data[start:i])
			lines = append(lines, line)
			start = i + 1
		}
	}
	if start < len(data) {
		lines = append(lines, string(data[start:]))
	}
	return lines
}

// Verify WG satisfies the interface at compile time.
var _ interface {
	Up(context.Context) error
	SetPeers(context.Context, []ployz.MachineRecord) error
	Down(context.Context) error
} = (*WG)(nil)
