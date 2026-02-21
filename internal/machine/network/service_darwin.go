//go:build darwin

package network

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"strings"

	"ployz/internal/platform/docker"
	"ployz/internal/platform/wireguard"

	"github.com/containerd/errdefs"
	"github.com/docker/docker/api/types/container"
	"github.com/docker/docker/api/types/image"
	dockernetwork "github.com/docker/docker/api/types/network"
	"github.com/docker/docker/client"
)

type linuxHelper struct {
	cli   *client.Client
	image string
	name  string
}

func New() (*Controller, error) {
	cli, err := client.NewClientWithOpts(client.FromEnv, client.WithAPIVersionNegotiation())
	if err != nil {
		return nil, fmt.Errorf("create docker client: %w", err)
	}
	return &Controller{cli: cli}, nil
}

type darwinRuntimeOps struct {
	ctrl   *Controller
	helper *linuxHelper
}

func (o darwinRuntimeOps) Prepare(ctx context.Context, _ Config) error {
	if err := docker.WaitReady(ctx, o.ctrl.cli); err != nil {
		return err
	}
	return o.helper.preflight(ctx)
}

func (o darwinRuntimeOps) ConfigureWireGuard(ctx context.Context, cfg Config, state *State) error {
	return configureWireGuardDarwin(ctx, o.helper, cfg, state, nil)
}

func (o darwinRuntimeOps) EnsureDockerNetwork(ctx context.Context, cfg Config, _ *State) error {
	bridge, err := docker.EnsureNetwork(ctx, o.ctrl.cli, cfg.DockerNetwork, cfg.Subnet, cfg.WGInterface)
	if err != nil {
		return err
	}
	return ensureIptablesRulesWithHelper(ctx, o.helper, cfg.WGInterface, bridge, cfg.Subnet.String())
}

func (o darwinRuntimeOps) CleanupDockerNetwork(ctx context.Context, cfg Config, state *State) error {
	bridge, err := docker.CleanupNetwork(ctx, o.ctrl.cli, cfg.DockerNetwork)
	if err != nil {
		return err
	}
	if bridge == "" {
		return nil
	}
	return cleanupIptablesRulesWithHelper(ctx, o.helper, state.WGInterface, bridge, state.Subnet)
}

func (o darwinRuntimeOps) CleanupWireGuard(ctx context.Context, _ Config, state *State) error {
	return wireguard.CleanupWithHelper(ctx, o.helper.run, state.WGInterface)
}

func (o darwinRuntimeOps) AfterStop(ctx context.Context, _ Config, _ *State) error {
	return o.helper.stop(ctx)
}

func (c *Controller) Start(ctx context.Context, in Config) (Config, error) {
	cfg, err := NormalizeConfig(in)
	if err != nil {
		return Config{}, err
	}
	helper := &linuxHelper{cli: c.cli, image: cfg.HelperImage, name: cfg.HelperName}
	return c.startRuntime(ctx, in, darwinRuntimeOps{ctrl: c, helper: helper})
}

func (c *Controller) Stop(ctx context.Context, in Config, purge bool) (Config, error) {
	cfg, err := NormalizeConfig(in)
	if err != nil {
		return Config{}, err
	}
	helper := &linuxHelper{cli: c.cli, image: cfg.HelperImage, name: cfg.HelperName}
	return c.stopRuntime(ctx, in, purge, darwinRuntimeOps{ctrl: c, helper: helper})
}

func (c *Controller) Status(ctx context.Context, in Config) (Status, error) {
	cfg, err := NormalizeConfig(in)
	if err != nil {
		return Status{}, err
	}

	out := Status{StatePath: statePath(cfg.DataDir)}
	s, err := loadState(cfg.DataDir)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return out, nil
		}
		return Status{}, err
	}
	out.Configured = true
	out.Running = s.Running
	cfg, err = Resolve(cfg, s)
	if err != nil {
		return Status{}, err
	}

	if err := docker.WaitReady(ctx, c.cli); err == nil {
		helper := &linuxHelper{cli: c.cli, image: cfg.HelperImage, name: cfg.HelperName}
		if ok, checkErr := helper.interfaceExists(ctx, s.WGInterface); checkErr == nil {
			out.WireGuard = ok
		}
		if n, nErr := c.cli.NetworkInspect(ctx, s.DockerNetwork, dockernetwork.InspectOptions{}); nErr == nil && n.ID != "" {
			out.DockerNet = true
		}
		if ctr, cErr := c.cli.ContainerInspect(ctx, s.CorrosionName); cErr == nil && ctr.State != nil && ctr.State.Running {
			out.Corrosion = true
		}
	}

	return out, nil
}

func (c *Controller) applyPeerConfig(ctx context.Context, cfg Config, state *State, peers []Peer) error {
	helper := &linuxHelper{cli: c.cli, image: cfg.HelperImage, name: cfg.HelperName}
	if err := helper.preflight(ctx); err != nil {
		return err
	}
	return configureWireGuardDarwin(ctx, helper, cfg, state, peers)
}

func configureWireGuardDarwin(ctx context.Context, helper *linuxHelper, cfg Config, state *State, peers []Peer) error {
	specs, err := buildPeerSpecs(peers)
	if err != nil {
		return fmt.Errorf("build peer specs: %w", err)
	}
	wgPeers := make([]wireguard.PeerConfig, len(specs))
	for i, s := range specs {
		wgPeers[i] = wireguard.PeerConfig{
			PublicKey:       s.publicKey,
			Endpoint:        s.endpoint,
			AllowedPrefixes: s.allowedPrefixes,
		}
	}
	return wireguard.ConfigureWithHelper(ctx, helper.run,
		state.WGInterface, defaultWireGuardMTU, state.WGPrivate, state.WGPort,
		machineIP(cfg.Subnet), cfg.Management, wgPeers)
}

// linuxHelper manages the privileged Linux helper container on macOS.

func (h *linuxHelper) preflight(ctx context.Context) error {
	if err := h.ensureRunning(ctx); err != nil {
		return err
	}
	err := h.run(ctx, `set -eu
command -v ip >/dev/null
command -v wg >/dev/null
command -v iptables >/dev/null`)
	if err != nil {
		return fmt.Errorf("linux helper image %q missing required tools (ip/wg/iptables): %w", h.image, err)
	}
	return nil
}

func (h *linuxHelper) ensureRunning(ctx context.Context) error {
	inspect, err := h.cli.ContainerInspect(ctx, h.name)
	if err == nil {
		if inspect.State != nil && inspect.State.Running {
			return nil
		}
		if startErr := h.cli.ContainerStart(ctx, h.name, container.StartOptions{}); startErr != nil {
			return fmt.Errorf("start linux helper container %q: %w", h.name, startErr)
		}
		return nil
	}
	if !errdefs.IsNotFound(err) {
		return fmt.Errorf("inspect linux helper container %q: %w", h.name, err)
	}

	cfg := &container.Config{
		Image:      h.image,
		Entrypoint: []string{"sh", "-c"},
		Cmd:        []string{"while true; do sleep 3600; done"},
	}
	hostCfg := &container.HostConfig{
		NetworkMode: dockernetwork.NetworkHost,
		Privileged:  true,
		CapAdd:      []string{"NET_ADMIN", "NET_RAW", "SYS_MODULE"},
		RestartPolicy: container.RestartPolicy{
			Name: container.RestartPolicyUnlessStopped,
		},
	}

	_, err = h.cli.ContainerCreate(ctx, cfg, hostCfg, nil, nil, h.name)
	if err != nil {
		if !errdefs.IsNotFound(err) {
			return fmt.Errorf("create linux helper container %q: %w", h.name, err)
		}
		pull, pullErr := h.cli.ImagePull(ctx, h.image, image.PullOptions{})
		if pullErr != nil {
			return fmt.Errorf("pull helper image %q: %w", h.image, pullErr)
		}
		_, _ = io.Copy(io.Discard, pull)
		_ = pull.Close()
		if _, err = h.cli.ContainerCreate(ctx, cfg, hostCfg, nil, nil, h.name); err != nil {
			return fmt.Errorf("create linux helper container %q after pull: %w", h.name, err)
		}
	}
	if err = h.cli.ContainerStart(ctx, h.name, container.StartOptions{}); err != nil {
		return fmt.Errorf("start linux helper container %q: %w", h.name, err)
	}
	return nil
}

func (h *linuxHelper) stop(ctx context.Context) error {
	if err := h.cli.ContainerRemove(ctx, h.name, container.RemoveOptions{Force: true}); err != nil {
		if !errdefs.IsNotFound(err) {
			return fmt.Errorf("remove linux helper container %q: %w", h.name, err)
		}
	}
	return nil
}

func (h *linuxHelper) interfaceExists(ctx context.Context, iface string) (bool, error) {
	code, _, err := h.runStatus(ctx, fmt.Sprintf("set -eu\nip link show dev %q >/dev/null 2>&1", iface))
	if err != nil {
		return false, err
	}
	if code == 0 {
		return true, nil
	}
	if code == 1 {
		return false, nil
	}
	return false, fmt.Errorf("unexpected exit code checking interface %q: %d", iface, code)
}

func (h *linuxHelper) run(ctx context.Context, script string) error {
	code, out, err := h.runStatus(ctx, script)
	if err != nil {
		return err
	}
	if code != 0 {
		if out != "" {
			return fmt.Errorf("helper command failed with exit code %d: %s", code, out)
		}
		return fmt.Errorf("helper command failed with exit code %d", code)
	}
	return nil
}

func (h *linuxHelper) runStatus(ctx context.Context, script string) (int64, string, error) {
	if err := h.ensureRunning(ctx); err != nil {
		return 0, "", err
	}

	execResp, err := h.cli.ContainerExecCreate(ctx, h.name, container.ExecOptions{
		AttachStdout: true,
		AttachStderr: true,
		Cmd:          []string{"sh", "-c", script},
		Tty:          true,
	})
	if err != nil {
		return 0, "", fmt.Errorf("create helper exec: %w", err)
	}

	attach, err := h.cli.ContainerExecAttach(ctx, execResp.ID, container.ExecAttachOptions{})
	if err != nil {
		return 0, "", fmt.Errorf("attach helper exec: %w", err)
	}
	defer attach.Close()

	data, err := io.ReadAll(attach.Reader)
	if err != nil {
		return 0, "", fmt.Errorf("read helper exec output: %w", err)
	}

	inspectResult, err := h.cli.ContainerExecInspect(ctx, execResp.ID)
	if err != nil {
		return 0, "", fmt.Errorf("inspect helper exec: %w", err)
	}

	return int64(inspectResult.ExitCode), strings.TrimSpace(string(data)), nil
}

func ensureIptablesRulesWithHelper(ctx context.Context, helper *linuxHelper, wgIface, bridge, subnet string) error {
	script := fmt.Sprintf(`set -eu
iface=%q
bridge=%q
subnet=%q
iptables -L DOCKER-USER -n >/dev/null 2>&1 || iptables -N DOCKER-USER
iptables -C DOCKER-USER -i "$iface" -o "$bridge" -j ACCEPT >/dev/null 2>&1 || iptables -I DOCKER-USER 1 -i "$iface" -o "$bridge" -j ACCEPT
iptables -t nat -D POSTROUTING -s "$subnet" -o "$iface" -j RETURN >/dev/null 2>&1 || true
iptables -t nat -I POSTROUTING 1 -s "$subnet" -o "$iface" -j RETURN`, wgIface, bridge, subnet)
	if err := helper.run(ctx, script); err != nil {
		return fmt.Errorf("configure iptables through linux helper: %w", err)
	}
	return nil
}

func cleanupIptablesRulesWithHelper(ctx context.Context, helper *linuxHelper, wgIface, bridge, subnet string) error {
	script := fmt.Sprintf(`set -eu
iface=%q
bridge=%q
subnet=%q
iptables -D DOCKER-USER -i "$iface" -o "$bridge" -j ACCEPT >/dev/null 2>&1 || true
iptables -t nat -D POSTROUTING -s "$subnet" -o "$iface" -j RETURN >/dev/null 2>&1 || true`, wgIface, bridge, subnet)
	if err := helper.run(ctx, script); err != nil {
		return fmt.Errorf("cleanup iptables through linux helper: %w", err)
	}
	return nil
}
