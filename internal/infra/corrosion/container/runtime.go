package container

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/netip"
	"strconv"
	"strings"
	"time"

	"github.com/containerd/errdefs"
	"github.com/docker/docker/api/types/container"
	"github.com/docker/docker/api/types/image"
	"github.com/docker/docker/api/types/mount"
	dockernetwork "github.com/docker/docker/api/types/network"
	"github.com/docker/docker/client"
)

type RuntimeConfig struct {
	Name       string
	Image      string
	ConfigPath string
	DataDir    string
	User       string
	GossipAddr netip.AddrPort
	APIAddr    netip.AddrPort
	APIToken   string
}

func Start(ctx context.Context, cli *client.Client, cfg RuntimeConfig) error {
	log := slog.With("component", "corrosion-runtime", "container", cfg.Name)
	log.Info("starting")
	phase := ContainerNotPresent
	if err := validateGossipBindAddr(cfg.GossipAddr); err != nil {
		return fmt.Errorf("validate corrosion gossip bind address: %w", err)
	}
	_, err := cli.ContainerInspect(ctx, cfg.Name)
	if err == nil {
		phase = phase.Transition(ContainerRemovingStale)
		log.Debug("removing existing container")
		if err := cli.ContainerRemove(ctx, cfg.Name, container.RemoveOptions{Force: true}); err != nil && !isRemoveOK(err) {
			return fmt.Errorf("remove old corrosion container: %w", err)
		}
		if err := waitContainerRemoved(ctx, cli, cfg.Name, 30*time.Second); err != nil {
			return fmt.Errorf("wait for old corrosion container removal: %w", err)
		}
	} else if !errdefs.IsNotFound(err) {
		return fmt.Errorf("inspect corrosion container: %w", err)
	}

	phase = phase.Transition(ContainerCreating)
	if _, err := cli.ContainerCreate(ctx, containerConfig(cfg), hostConfig(cfg), nil, nil, cfg.Name); err != nil {
		if !errdefs.IsNotFound(err) {
			return wrapCorrosionDataPermissionError("create corrosion container", cfg.DataDir, err)
		}
		phase = phase.Transition(ContainerPullingImage)
		log.Info("pulling image", "image", cfg.Image)
		pull, pullErr := cli.ImagePull(ctx, cfg.Image, image.PullOptions{})
		if pullErr != nil {
			return fmt.Errorf("pull corrosion image: %w", pullErr)
		}
		_, _ = io.Copy(io.Discard, pull) // drain pull stream to completion
		_ = pull.Close()                 // best-effort cleanup
		phase = phase.Transition(ContainerCreating)
		if _, err = cli.ContainerCreate(ctx, containerConfig(cfg), hostConfig(cfg), nil, nil, cfg.Name); err != nil {
			return wrapCorrosionDataPermissionError("create corrosion container after pull", cfg.DataDir, err)
		}
	}

	phase = phase.Transition(ContainerStarting)
	if err := cli.ContainerStart(ctx, cfg.Name, container.StartOptions{}); err != nil {
		return fmt.Errorf("start corrosion container: %w", err)
	}
	log.Info("container started")
	phase = phase.Transition(ContainerWaitingReady)
	if err := waitReady(ctx, cli, cfg.Name, cfg.APIAddr, cfg.APIToken, cfg.DataDir, 30*time.Second); err != nil {
		return err
	}
	log.Info("api ready", "api_addr", cfg.APIAddr.String())
	phase = phase.Transition(ContainerApplyingSchema)
	if err := applySchema(ctx, cfg.APIAddr, cfg.APIToken); err != nil {
		return err
	}
	phase = phase.Transition(ContainerOperational)
	log.Info("schema applied")
	log.Debug("container lifecycle phase", "phase", phase.String())
	return nil
}

func Stop(ctx context.Context, cli *client.Client, name string) error {
	slog.Info("stopping corrosion runtime", "component", "corrosion-runtime", "container", name)
	if err := cli.ContainerStop(ctx, name, container.StopOptions{}); err != nil && !isRemoveOK(err) {
		return fmt.Errorf("stop corrosion container: %w", err)
	}
	if err := cli.ContainerRemove(ctx, name, container.RemoveOptions{Force: true}); err != nil && !isRemoveOK(err) {
		return fmt.Errorf("remove corrosion container: %w", err)
	}
	return nil
}

func isRemoveOK(err error) bool {
	return err == nil || errdefs.IsNotFound(err) || isRemovalInProgress(err)
}

func isRemovalInProgress(err error) bool {
	if err == nil {
		return false
	}
	msg := strings.ToLower(err.Error())
	return strings.Contains(msg, "already in progress") ||
		strings.Contains(msg, "already being removed") ||
		strings.Contains(msg, "marked for removal")
}

func wrapCorrosionDataPermissionError(action, dataDir string, err error) error {
	if err == nil {
		return nil
	}
	msg := strings.ToLower(err.Error())
	if strings.Contains(msg, "permission denied") {
		return fmt.Errorf("%s: %w (corrosion data dir %s may be inaccessible to docker backend; run `sudo ployz configure`)", action, err, dataDir)
	}
	return fmt.Errorf("%s: %w", action, err)
}

func corrosionDataPermissionHint(logs, dataDir string) string {
	lower := strings.ToLower(logs)
	if strings.Contains(lower, "unable to open database file") || strings.Contains(lower, "error code 14") || strings.Contains(lower, "permission denied") {
		return fmt.Sprintf("corrosion data directory %s is not writable; run `sudo ployz configure` to reconcile permissions", dataDir)
	}
	return ""
}

func waitContainerRemoved(ctx context.Context, cli *client.Client, name string, timeout time.Duration) error {
	deadline := time.NewTimer(timeout)
	defer deadline.Stop()
	ticker := time.NewTicker(250 * time.Millisecond)
	defer ticker.Stop()

	for {
		_, err := cli.ContainerInspect(ctx, name)
		switch {
		case err == nil:
		case errdefs.IsNotFound(err):
			return nil
		case isRemovalInProgress(err):
		default:
			return fmt.Errorf("inspect corrosion container: %w", err)
		}

		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-deadline.C:
			return fmt.Errorf("timeout after %s waiting for container %q removal", timeout, name)
		case <-ticker.C:
		}
	}
}

func validateGossipBindAddr(gossipAddr netip.AddrPort) error {
	localAddrs, err := localInterfaceAddrs()
	if err != nil {
		return fmt.Errorf("list host interface addresses: %w", err)
	}
	if err := validateGossipBindAddrWithLocalAddrs(gossipAddr, localAddrs); err != nil {
		return err
	}
	return nil
}

func validateGossipBindAddrWithLocalAddrs(gossipAddr netip.AddrPort, localAddrs []netip.Addr) error {
	bindAddr := gossipAddr.Addr()
	if !bindAddr.IsValid() {
		return fmt.Errorf("corrosion gossip bind address is invalid")
	}
	if bindAddr.IsUnspecified() {
		return nil
	}
	bindAddr = bindAddr.Unmap()
	for _, localAddr := range localAddrs {
		if !localAddr.IsValid() {
			continue
		}
		if localAddr.Unmap() == bindAddr {
			return nil
		}
	}
	return fmt.Errorf("corrosion gossip bind address %s is not assigned on this host; ensure wireguard management IP is configured before starting corrosion", bindAddr)
}

func localInterfaceAddrs() ([]netip.Addr, error) {
	ifaceAddrs, err := net.InterfaceAddrs()
	if err != nil {
		return nil, err
	}
	addrs := make([]netip.Addr, 0, len(ifaceAddrs))
	for _, addr := range ifaceAddrs {
		ip, ok := netipAddrFromInterfaceAddr(addr)
		if !ok {
			continue
		}
		addrs = append(addrs, ip)
	}
	return addrs, nil
}

func netipAddrFromInterfaceAddr(addr net.Addr) (netip.Addr, bool) {
	var ip net.IP
	switch typed := addr.(type) {
	case *net.IPNet:
		ip = typed.IP
	case *net.IPAddr:
		ip = typed.IP
	default:
		return netip.Addr{}, false
	}
	parsed, ok := netip.AddrFromSlice(ip)
	if !ok {
		return netip.Addr{}, false
	}
	return parsed.Unmap(), true
}

func waitReady(ctx context.Context, cli *client.Client, name string, apiAddr netip.AddrPort, apiToken, dataDir string, timeout time.Duration) error {
	log := slog.With("component", "corrosion-runtime", "container", name, "api_addr", apiAddr.String())
	httpCli := &http.Client{Timeout: 2 * time.Second}
	body := []byte(`{"query":"SELECT 1","params":[]}`)
	url := "http://" + apiAddr.String() + "/v1/queries"

	deadline := time.NewTimer(timeout)
	defer deadline.Stop()
	ticker := time.NewTicker(500 * time.Millisecond)
	defer ticker.Stop()

	var lastErr string
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-deadline.C:
			msg := "corrosion not ready after " + timeout.String()
			if lastErr != "" {
				msg += ": " + lastErr
			}
			if logs := containerLogs(ctx, cli, name, 10); logs != "" {
				msg += "\n" + logs
				if hint := corrosionDataPermissionHint(logs, dataDir); hint != "" {
					msg += "\n" + hint
				}
			}
			log.Warn("readiness timeout", "detail", msg)
			return fmt.Errorf("%s", msg)
		case <-ticker.C:
			// fail fast if container exited
			info, inspectErr := cli.ContainerInspect(ctx, name)
			if inspectErr != nil {
				lastErr = "container not found"
				continue
			}
			if !info.State.Running {
				msg := fmt.Sprintf("corrosion container exited (status %d)", info.State.ExitCode)
				if logs := containerLogs(ctx, cli, name, 20); logs != "" {
					msg += "\n" + logs
					if hint := corrosionDataPermissionHint(logs, dataDir); hint != "" {
						msg += "\n" + hint
					}
				}
				log.Error("container exited before readiness", "exit_code", info.State.ExitCode)
				return fmt.Errorf("%s", msg)
			}

			req, err := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewReader(body))
			if err != nil {
				return fmt.Errorf("create readiness request: %w", err)
			}
			req.Header.Set("Content-Type", "application/json")
			req.Header.Set("Accept", "application/json")
			if apiToken != "" {
				req.Header.Set("Authorization", "Bearer "+apiToken)
			}

			resp, err := httpCli.Do(req)
			if err != nil {
				lastErr = err.Error()
				continue
			}

			if resp.StatusCode != http.StatusOK {
				data, _ := io.ReadAll(resp.Body) // best-effort error body
				_ = resp.Body.Close()            // best-effort cleanup
				lastErr = fmt.Sprintf("status %d: %s", resp.StatusCode, bytes.TrimSpace(data))
				continue
			}

			var event struct {
				Error *string `json:"error"`
			}
			err = json.NewDecoder(resp.Body).Decode(&event)
			_ = resp.Body.Close() // best-effort cleanup
			if err != nil {
				lastErr = "decode response: " + err.Error()
				continue
			}
			if event.Error != nil {
				lastErr = *event.Error
				continue
			}
			log.Debug("readiness probe succeeded")
			return nil
		}
	}
}

func applySchema(ctx context.Context, apiAddr netip.AddrPort, apiToken string) error {
	stmts := []string{
		"CREATE TABLE IF NOT EXISTS cluster (key TEXT NOT NULL PRIMARY KEY, value ANY)",
		"CREATE TABLE IF NOT EXISTS network_config (key TEXT NOT NULL PRIMARY KEY, value TEXT NOT NULL DEFAULT '')",
		"CREATE TABLE IF NOT EXISTS machines (id TEXT NOT NULL PRIMARY KEY, public_key TEXT NOT NULL DEFAULT '', subnet TEXT NOT NULL DEFAULT '', management_ip TEXT NOT NULL DEFAULT '', endpoint TEXT NOT NULL DEFAULT '', updated_at TEXT NOT NULL DEFAULT '', version INTEGER NOT NULL DEFAULT 1)",
		"CREATE TABLE IF NOT EXISTS heartbeats (node_id TEXT NOT NULL PRIMARY KEY, seq INTEGER NOT NULL DEFAULT 0, updated_at TEXT NOT NULL DEFAULT '')",
	}
	body, err := json.Marshal(stmts)
	if err != nil {
		return fmt.Errorf("marshal schema: %w", err)
	}
	url := "http://" + apiAddr.String() + "/v1/migrations"
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewReader(body))
	if err != nil {
		return fmt.Errorf("create schema request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Accept", "application/json")
	if apiToken != "" {
		req.Header.Set("Authorization", "Bearer "+apiToken)
	}
	resp, err := (&http.Client{Timeout: 10 * time.Second}).Do(req)
	if err != nil {
		return fmt.Errorf("apply schema: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		data, _ := io.ReadAll(resp.Body) // best-effort error body
		return fmt.Errorf("apply schema: status %d: %s", resp.StatusCode, bytes.TrimSpace(data))
	}

	var out struct {
		Results []struct {
			Error *string `json:"error"`
		} `json:"results"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&out); err != nil {
		return fmt.Errorf("decode schema response: %w", err)
	}
	for _, result := range out.Results {
		if result.Error != nil && strings.TrimSpace(*result.Error) != "" {
			return fmt.Errorf("apply schema: %s", *result.Error)
		}
	}
	return nil
}

func containerLogs(ctx context.Context, cli *client.Client, name string, lines int) string {
	opts := container.LogsOptions{ShowStdout: true, ShowStderr: true, Tail: strconv.Itoa(lines)}
	rc, err := cli.ContainerLogs(ctx, name, opts)
	if err != nil {
		return ""
	}
	defer rc.Close()
	data, _ := io.ReadAll(rc) // best-effort; log output may be truncated on error
	// strip docker stream framing (8-byte header per chunk)
	var clean []byte
	for len(data) >= 8 {
		size := int(data[4])<<24 | int(data[5])<<16 | int(data[6])<<8 | int(data[7])
		data = data[8:]
		if size > len(data) {
			size = len(data)
		}
		clean = append(clean, data[:size]...)
		data = data[size:]
	}
	return string(bytes.TrimSpace(clean))
}

func containerConfig(cfg RuntimeConfig) *container.Config {
	return &container.Config{
		Image: cfg.Image,
		Cmd:   []string{"corrosion", "agent", "-c", cfg.ConfigPath},
		User:  cfg.User,
	}
}

func hostConfig(cfg RuntimeConfig) *container.HostConfig {
	return &container.HostConfig{
		NetworkMode: dockernetwork.NetworkHost,
		RestartPolicy: container.RestartPolicy{
			Name: container.RestartPolicyAlways,
		},
		Mounts: []mount.Mount{
			{
				Type:   mount.TypeBind,
				Source: cfg.DataDir,
				Target: cfg.DataDir,
			},
		},
	}
}
