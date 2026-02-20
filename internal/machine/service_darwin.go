//go:build darwin

package machine

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/netip"
	"os"
	"strings"

	"ployz/internal/machine/dockerutil"

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
	if err := waitDockerReady(ctx, o.ctrl.cli); err != nil {
		return err
	}
	return o.helper.preflight(ctx)
}

func (o darwinRuntimeOps) ConfigureWireGuard(ctx context.Context, cfg Config, state *State) error {
	return configureWireGuardWithHelper(ctx, o.helper, cfg, state, nil)
}

func (o darwinRuntimeOps) EnsureDockerNetwork(ctx context.Context, cfg Config, _ *State) error {
	return ensureDockerNetworkWithHelper(ctx, o.ctrl.cli, o.helper, cfg)
}

func (o darwinRuntimeOps) CleanupDockerNetwork(ctx context.Context, cfg Config, state *State) error {
	return cleanupDockerNetworkWithHelper(ctx, o.ctrl.cli, o.helper, cfg, state)
}

func (o darwinRuntimeOps) CleanupWireGuard(ctx context.Context, _ Config, state *State) error {
	return cleanupWireGuardWithHelper(ctx, o.helper, state.WGInterface)
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

	if err := waitDockerReady(ctx, c.cli); err == nil {
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
	return configureWireGuardWithHelper(ctx, helper, cfg, state, peers)
}

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
		Entrypoint: []string{"sh", "-lc"},
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
		Cmd:          []string{"sh", "-lc", script},
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

	inspect, err := h.cli.ContainerExecInspect(ctx, execResp.ID)
	if err != nil {
		return 0, "", fmt.Errorf("inspect helper exec: %w", err)
	}

	return int64(inspect.ExitCode), strings.TrimSpace(string(data)), nil
}

func configureWireGuardWithHelper(ctx context.Context, helper *linuxHelper, cfg Config, state *State, peers []Peer) error {
	specs, err := buildPeerSpecs(peers)
	if err != nil {
		return fmt.Errorf("build peer specs: %w", err)
	}

	var script strings.Builder
	localMachineIP := machineIP(cfg.Subnet)
	script.WriteString("set -eu\n")
	fmt.Fprintf(&script, "iface=%q\n", state.WGInterface)
	fmt.Fprintf(&script, "priv=%q\n", state.WGPrivate)
	fmt.Fprintf(&script, "port=%d\n", state.WGPort)
	fmt.Fprintf(&script, "machine_addr=%q\n", localMachineIP.String())
	fmt.Fprintf(&script, "machine_bits=%d\n", addrBits(localMachineIP))
	fmt.Fprintf(&script, "mgmt_addr=%q\n", cfg.Management.String())
	fmt.Fprintf(&script, "mgmt_bits=%d\n", addrBits(cfg.Management))
	script.WriteString("modprobe wireguard >/dev/null 2>&1 || true\n")
	script.WriteString("if ! ip link show \"$iface\" >/dev/null 2>&1; then\n")
	script.WriteString("  ip link add dev \"$iface\" type wireguard\n")
	script.WriteString("fi\n")
	fmt.Fprintf(&script, "ip link set dev \"$iface\" mtu %d\n", defaultWireGuardMTU)
	script.WriteString("tmp=$(mktemp)\n")
	script.WriteString("trap 'rm -f \"$tmp\"' EXIT\n")
	script.WriteString("printf '%s' \"$priv\" > \"$tmp\"\n")
	script.WriteString("wg set \"$iface\" listen-port \"$port\" private-key \"$tmp\"\n")

	script.WriteString("desired_keys=\"")
	for i, spec := range specs {
		if i > 0 {
			script.WriteString(" ")
		}
		script.WriteString(spec.publicKeyString)
	}
	script.WriteString("\"\n")
	script.WriteString("for k in $(wg show \"$iface\" peers 2>/dev/null || true); do\n")
	script.WriteString("  keep=0\n")
	script.WriteString("  for d in $desired_keys; do\n")
	script.WriteString("    if [ \"$k\" = \"$d\" ]; then keep=1; break; fi\n")
	script.WriteString("  done\n")
	script.WriteString("  if [ \"$keep\" -eq 0 ]; then wg set \"$iface\" peer \"$k\" remove; fi\n")
	script.WriteString("done\n")

	for _, spec := range specs {
		allowed := make([]string, len(spec.allowedPrefixes))
		for i, pref := range spec.allowedPrefixes {
			allowed[i] = pref.String()
		}
		allowedStr := strings.Join(allowed, ",")
		if spec.endpoint != nil {
			fmt.Fprintf(
				&script,
				"wg set \"$iface\" peer %q endpoint %q persistent-keepalive 25 allowed-ips %q\n",
				spec.publicKeyString,
				spec.endpoint.String(),
				allowedStr,
			)
		} else {
			fmt.Fprintf(
				&script,
				"wg set \"$iface\" peer %q persistent-keepalive 25 allowed-ips %q\n",
				spec.publicKeyString,
				allowedStr,
			)
		}
		for _, pref := range spec.allowedPrefixes {
			routeCmd := "ip route replace"
			if pref.Addr().Is6() {
				routeCmd = "ip -6 route replace"
			}
			fmt.Fprintf(&script, "%s %q dev \"$iface\"\n", routeCmd, pref.String())
		}
	}

	script.WriteString("ip addr replace \"$machine_addr/$machine_bits\" dev \"$iface\"\n")
	script.WriteString("ip addr replace \"$mgmt_addr/$mgmt_bits\" dev \"$iface\"\n")
	script.WriteString("ip link set up dev \"$iface\"\n")

	if err := helper.run(ctx, script.String()); err != nil {
		return fmt.Errorf("configure wireguard through linux helper: %w", err)
	}
	return nil
}

func cleanupWireGuardWithHelper(ctx context.Context, helper *linuxHelper, iface string) error {
	script := fmt.Sprintf(`set -eu
iface=%q
ip link del dev "$iface" >/dev/null 2>&1 || true`, iface)
	if err := helper.run(ctx, script); err != nil {
		return fmt.Errorf("cleanup wireguard through linux helper: %w", err)
	}
	return nil
}

func addrBits(addr netip.Addr) int {
	if addr.Is6() {
		return 128
	}
	return 32
}

func ensureDockerNetworkWithHelper(ctx context.Context, cli *client.Client, helper *linuxHelper, cfg Config) error {
	needsCreate := false
	nw, err := cli.NetworkInspect(ctx, cfg.DockerNetwork, dockernetwork.InspectOptions{})
	if err != nil {
		if !errdefs.IsNotFound(err) {
			return fmt.Errorf("inspect docker network %q: %w", cfg.DockerNetwork, err)
		}
		needsCreate = true
	} else if len(nw.IPAM.Config) == 0 || nw.IPAM.Config[0].Subnet != cfg.Subnet.String() {
		if err := dockerutil.PurgeNetworkContainers(ctx, cli, cfg.DockerNetwork, nw); err != nil {
			return err
		}
		if err := cli.NetworkRemove(ctx, cfg.DockerNetwork); err != nil {
			return fmt.Errorf("remove old docker network %q: %w", cfg.DockerNetwork, err)
		}
		needsCreate = true
	}

	if needsCreate {
		if _, err := cli.NetworkCreate(ctx, cfg.DockerNetwork, dockernetwork.CreateOptions{
			Driver: "bridge",
			Scope:  "local",
			IPAM:   &dockernetwork.IPAM{Config: []dockernetwork.IPAMConfig{{Subnet: cfg.Subnet.String()}}},
			Options: map[string]string{
				"com.docker.network.bridge.trusted_host_interfaces": cfg.WGInterface,
			},
		}); err != nil {
			return fmt.Errorf("create docker network %q: %w", cfg.DockerNetwork, err)
		}
		nw, err = cli.NetworkInspect(ctx, cfg.DockerNetwork, dockernetwork.InspectOptions{})
		if err != nil {
			return fmt.Errorf("inspect docker network %q: %w", cfg.DockerNetwork, err)
		}
	}

	bridge := "br-" + nw.ID[:12]
	return ensureIptablesRulesWithHelper(ctx, helper, cfg, bridge)
}

func cleanupDockerNetworkWithHelper(
	ctx context.Context,
	cli *client.Client,
	helper *linuxHelper,
	cfg Config,
	state *State,
) error {
	nw, err := cli.NetworkInspect(ctx, cfg.DockerNetwork, dockernetwork.InspectOptions{})
	if err != nil {
		if errdefs.IsNotFound(err) {
			return nil
		}
		return fmt.Errorf("inspect docker network %q: %w", cfg.DockerNetwork, err)
	}
	if err := dockerutil.PurgeNetworkContainers(ctx, cli, cfg.DockerNetwork, nw); err != nil {
		return err
	}
	bridge := "br-" + nw.ID[:12]
	if err := cleanupIptablesRulesWithHelper(ctx, helper, state, bridge); err != nil {
		return err
	}
	if err := cli.NetworkRemove(ctx, cfg.DockerNetwork); err != nil && !errdefs.IsNotFound(err) {
		return fmt.Errorf("remove docker network %q: %w", cfg.DockerNetwork, err)
	}
	return nil
}

func ensureIptablesRulesWithHelper(ctx context.Context, helper *linuxHelper, cfg Config, bridge string) error {
	script := fmt.Sprintf(`set -eu
iface=%q
bridge=%q
subnet=%q
iptables -C DOCKER-USER -i "$iface" -o "$bridge" -j ACCEPT >/dev/null 2>&1 || iptables -I DOCKER-USER 1 -i "$iface" -o "$bridge" -j ACCEPT
iptables -t nat -D POSTROUTING -s "$subnet" -o "$iface" -j RETURN >/dev/null 2>&1 || true
iptables -t nat -I POSTROUTING 1 -s "$subnet" -o "$iface" -j RETURN`, cfg.WGInterface, bridge, cfg.Subnet.String())
	if err := helper.run(ctx, script); err != nil {
		return fmt.Errorf("configure iptables through linux helper: %w", err)
	}
	return nil
}

func cleanupIptablesRulesWithHelper(ctx context.Context, helper *linuxHelper, state *State, bridge string) error {
	subnet, err := netip.ParsePrefix(state.Subnet)
	if err != nil {
		return fmt.Errorf("parse subnet from state: %w", err)
	}

	script := fmt.Sprintf(`set -eu
iface=%q
bridge=%q
subnet=%q
iptables -D DOCKER-USER -i "$iface" -o "$bridge" -j ACCEPT >/dev/null 2>&1 || true
iptables -t nat -D POSTROUTING -s "$subnet" -o "$iface" -j RETURN >/dev/null 2>&1 || true`, state.WGInterface, bridge, subnet.String())
	if err := helper.run(ctx, script); err != nil {
		return fmt.Errorf("cleanup iptables through linux helper: %w", err)
	}
	return nil
}
