//go:build darwin

package configure

import (
	"bytes"
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"text/template"
	"time"
)

const (
	helperLabel             = "com.ployz.helper"
	helperLaunchdDir        = "/Library/LaunchDaemons"
	helperStartupWait       = 5 * time.Second
	helperLogPath           = "/var/log/ployz-helper.log"
	helperServiceNameDarwin = "com.ployz.helper"
)

type darwinHelperService struct{}

func newPlatformHelperService() HelperService {
	return &darwinHelperService{}
}

func (s *darwinHelperService) Configure(ctx context.Context, opts HelperOptions) error {
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

	exe, err := resolvePloyzBinary()
	if err != nil {
		return fmt.Errorf("resolve ployz binary: %w", err)
	}

	if err := os.MkdirAll(helperLaunchdDir, 0o755); err != nil {
		return fmt.Errorf("create LaunchDaemons dir: %w", err)
	}

	plistBytes, err := renderHelperPlist(helperPlistData{
		Label:         helperLabel,
		Program:       exe,
		SocketPath:    opts.PrivSocketPath,
		TokenFilePath: opts.TokenPath,
		TUNSocketPath: opts.TUNSocketPath,
		MTU:           opts.MTU,
		LogPath:       helperLogPath,
	})
	if err != nil {
		return fmt.Errorf("render helper plist: %w", err)
	}
	plistPath := filepath.Join(helperLaunchdDir, helperLabel+".plist")

	_ = launchctlBootoutHelper(ctx, helperLabel)
	waitHelperUnloaded(ctx, helperLabel)

	if err := os.WriteFile(plistPath, plistBytes, 0o644); err != nil {
		return fmt.Errorf("write helper plist: %w", err)
	}

	if err := launchctlBootstrapHelper(ctx, helperLabel, plistPath); err != nil {
		return err
	}
	if err := waitForHelperSocket(ctx, opts.PrivSocketPath, helperStartupWait); err != nil {
		return err
	}

	return nil
}

func (s *darwinHelperService) Status(ctx context.Context) (HelperStatus, error) {
	out, cmdErr := exec.CommandContext(ctx, "launchctl", "print", "system/"+helperServiceNameDarwin).CombinedOutput()
	if cmdErr != nil {
		msg := strings.TrimSpace(string(out))
		if strings.Contains(strings.ToLower(msg), "could not find service") || strings.Contains(strings.ToLower(msg), "unknown service") {
			return HelperStatus{}, nil
		}
		return HelperStatus{}, fmt.Errorf("launchctl print helper status: %s: %w", msg, cmdErr)
	}
	return HelperStatus{
		Installed: true,
		Running:   strings.Contains(string(out), "state = running"),
	}, nil
}

func launchctlBootstrapHelper(ctx context.Context, label, plistPath string) error {
	out, err := exec.CommandContext(ctx, "launchctl", "bootstrap", "system", plistPath).CombinedOutput()
	if err == nil {
		return nil
	}
	_ = launchctlBootoutHelper(ctx, label)
	waitHelperUnloaded(ctx, label)
	out, err = exec.CommandContext(ctx, "launchctl", "bootstrap", "system", plistPath).CombinedOutput()
	if err != nil {
		return fmt.Errorf("bootstrap helper service: %s: %w", strings.TrimSpace(string(out)), err)
	}
	return nil
}

func launchctlBootoutHelper(ctx context.Context, label string) error {
	out, err := exec.CommandContext(ctx, "launchctl", "bootout", "system/"+label).CombinedOutput()
	if err != nil {
		return fmt.Errorf("bootout helper service: %s: %w", strings.TrimSpace(string(out)), err)
	}
	return nil
}

// waitHelperUnloaded polls until launchd reports the helper service is gone.
// Prevents bootstrap from racing against a still-unloading service.
func waitHelperUnloaded(ctx context.Context, label string) {
	const maxWait = 5 * time.Second
	const pollInterval = 100 * time.Millisecond
	deadline := time.Now().Add(maxWait)
	for time.Now().Before(deadline) {
		if err := exec.CommandContext(ctx, "launchctl", "print", "system/"+label).Run(); err != nil {
			return // service is gone
		}
		select {
		case <-ctx.Done():
			return
		case <-time.After(pollInterval):
		}
	}
}

type helperPlistData struct {
	Label         string
	Program       string
	SocketPath    string
	TokenFilePath string
	TUNSocketPath string
	MTU           int
	LogPath       string
}

var helperPlistTemplate = template.Must(template.New("helper_plist").Parse(`<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{{.Label}}</string>

  <key>ProgramArguments</key>
  <array>
    <string>{{.Program}}</string>
    <string>daemon</string>
    <string>helper</string>
    <string>--socket</string>
    <string>{{.SocketPath}}</string>
    <string>--token-file</string>
    <string>{{.TokenFilePath}}</string>
    <string>--tun-socket</string>
    <string>{{.TUNSocketPath}}</string>
    <string>--mtu</string>
    <string>{{.MTU}}</string>
  </array>

  <key>RunAtLoad</key>
  <true/>

  <key>KeepAlive</key>
  <true/>

  <key>StandardOutPath</key>
  <string>{{.LogPath}}</string>

  <key>StandardErrorPath</key>
  <string>{{.LogPath}}</string>
</dict>
</plist>
`))

func renderHelperPlist(data helperPlistData) ([]byte, error) {
	var buf bytes.Buffer
	if err := helperPlistTemplate.Execute(&buf, data); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

func helperSummary(opts HelperOptions) string {
	return fmt.Sprintf("socket=%s token=%s tun=%s mtu=%s", opts.PrivSocketPath, opts.TokenPath, opts.TUNSocketPath, strconv.Itoa(opts.MTU))
}
