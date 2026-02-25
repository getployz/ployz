//go:build linux

package configure

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"strings"
	"time"
)

const (
	helperUnitName         = "ployz-helper.service"
	helperUnitPath         = "/etc/systemd/system/" + helperUnitName
	helperStartupWait      = 5 * time.Second
	helperServiceNameLinux = "ployz-helper.service"
)

type linuxHelperService struct{}

func newPlatformHelperService() HelperService {
	return &linuxHelperService{}
}

func (s *linuxHelperService) Configure(ctx context.Context, opts HelperOptions) error {
	if os.Geteuid() != 0 {
		return fmt.Errorf("configure requires root privileges; run `sudo ployz configure`")
	}
	if opts.MTU <= 0 {
		return fmt.Errorf("invalid mtu %d", opts.MTU)
	}
	if err := removeStaleSocketPath(opts.PrivSocketPath); err != nil {
		return fmt.Errorf("remove stale helper socket: %w", err)
	}
	if err := removeStaleSocketPath(opts.TUNSocketPath); err != nil {
		return fmt.Errorf("remove stale tun socket: %w", err)
	}

	if _, err := ensureHelperToken(opts.TokenPath); err != nil {
		return err
	}

	ployzBin, err := resolvePloyzBinary()
	if err != nil {
		return fmt.Errorf("resolve ployz binary: %w", err)
	}

	unitContent := fmt.Sprintf(`[Unit]
Description=Ployz privileged helper
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=%s daemon helper --socket %s --token-file %s --tun-socket %s --mtu %d
User=root
Group=root
RuntimeDirectory=ployz
RuntimeDirectoryMode=0755
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
`, ployzBin, opts.PrivSocketPath, opts.TokenPath, opts.TUNSocketPath, opts.MTU)

	if err := os.WriteFile(helperUnitPath, []byte(unitContent), 0o644); err != nil {
		return fmt.Errorf("write helper systemd unit: %w", err)
	}

	if err := helperSystemctl(ctx, "daemon-reload"); err != nil {
		return err
	}
	if err := helperSystemctl(ctx, "enable", "--now", helperUnitName); err != nil {
		return err
	}
	if err := helperSystemctl(ctx, "restart", helperUnitName); err != nil {
		return fmt.Errorf("restart helper service: %w", err)
	}
	if err := waitForHelperSocket(ctx, opts.PrivSocketPath, helperStartupWait); err != nil {
		return err
	}

	return nil
}

func (s *linuxHelperService) Status(ctx context.Context) (HelperStatus, error) {
	loadStateOut, loadErr := exec.CommandContext(ctx, "systemctl", "show", helperServiceNameLinux, "--property", "LoadState", "--value").CombinedOutput()
	if loadErr != nil {
		return HelperStatus{}, fmt.Errorf("systemctl show LoadState: %s: %w", strings.TrimSpace(string(loadStateOut)), loadErr)
	}
	loadState := strings.TrimSpace(string(loadStateOut))
	installed := loadState == "loaded"

	activeStateOut, activeErr := exec.CommandContext(ctx, "systemctl", "show", helperServiceNameLinux, "--property", "ActiveState", "--value").CombinedOutput()
	if activeErr != nil {
		return HelperStatus{Installed: installed}, fmt.Errorf("systemctl show ActiveState: %s: %w", strings.TrimSpace(string(activeStateOut)), activeErr)
	}
	activeState := strings.TrimSpace(string(activeStateOut))

	return HelperStatus{Installed: installed, Running: activeState == "active"}, nil
}

func helperSystemctl(ctx context.Context, args ...string) error {
	out, err := exec.CommandContext(ctx, "systemctl", args...).CombinedOutput()
	if err != nil {
		return fmt.Errorf("systemctl %s: %s: %w", strings.Join(args, " "), strings.TrimSpace(string(out)), err)
	}
	return nil
}
