package agent

import (
	"bytes"
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"text/template"

	"ployz/cmd/ployz/cmdutil"
)

const (
	daemonLabel  = "com.ployz.ployzd"
	runtimeLabel = "com.ployz.ployz-runtime"
)

type darwinService struct{}

func NewPlatformService() PlatformService {
	return &darwinService{}
}

func (d *darwinService) Install(ctx context.Context, cfg InstallConfig) error {
	agentsDir, err := launchAgentsDir()
	if err != nil {
		return err
	}
	if err := os.MkdirAll(agentsDir, 0o755); err != nil {
		return fmt.Errorf("create LaunchAgents dir: %w", err)
	}
	if err := os.MkdirAll(cfg.DataRoot, 0o755); err != nil {
		return fmt.Errorf("create data root: %w", err)
	}

	ployzBin, err := resolveBinary("ployz")
	if err != nil {
		return fmt.Errorf("resolve ployz binary: %w", err)
	}

	daemonPlist, err := renderPlist(daemonPlistTmpl, plistData{
		Label:      daemonLabel,
		Program:    ployzBin,
		Args:       []string{"daemon", "run", "--socket", cfg.SocketPath, "--data-root", cfg.DataRoot},
		LogPath:    cmdutil.DaemonLogPath(cfg.DataRoot),
		RunAtLoad:  true,
		KeepAlive:  true,
	})
	if err != nil {
		return fmt.Errorf("render daemon plist: %w", err)
	}

	runtimeBin := resolveRuntimeBinary(ployzBin)
	runtimeArgs := runtimeBinArgs(runtimeBin, ployzBin, cfg.DataRoot)

	runtimePlist, err := renderPlist(runtimePlistTmpl, plistData{
		Label:      runtimeLabel,
		Program:    runtimeArgs[0],
		Args:       runtimeArgs[1:],
		LogPath:    cmdutil.RuntimeLogPath(cfg.DataRoot),
		RunAtLoad:  true,
		KeepAlive:  true,
	})
	if err != nil {
		return fmt.Errorf("render runtime plist: %w", err)
	}

	daemonPath := filepath.Join(agentsDir, daemonLabel+".plist")
	runtimePath := filepath.Join(agentsDir, runtimeLabel+".plist")

	// Idempotent: bootout before bootstrap
	_ = launchctlBootout(ctx, daemonLabel)
	_ = launchctlBootout(ctx, runtimeLabel)

	if err := os.WriteFile(daemonPath, daemonPlist, 0o644); err != nil {
		return fmt.Errorf("write daemon plist: %w", err)
	}
	if err := os.WriteFile(runtimePath, runtimePlist, 0o644); err != nil {
		return fmt.Errorf("write runtime plist: %w", err)
	}

	if err := launchctlBootstrap(ctx, daemonLabel, daemonPath); err != nil {
		return fmt.Errorf("bootstrap daemon: %w", err)
	}
	if err := launchctlBootstrap(ctx, runtimeLabel, runtimePath); err != nil {
		return fmt.Errorf("bootstrap runtime: %w", err)
	}

	return nil
}

func (d *darwinService) Uninstall(ctx context.Context) error {
	agentsDir, err := launchAgentsDir()
	if err != nil {
		return err
	}

	_ = launchctlBootout(ctx, daemonLabel)
	_ = launchctlBootout(ctx, runtimeLabel)

	os.Remove(filepath.Join(agentsDir, daemonLabel+".plist"))
	os.Remove(filepath.Join(agentsDir, runtimeLabel+".plist"))
	return nil
}

func (d *darwinService) Status(ctx context.Context) (ServiceStatus, error) {
	return ServiceStatus{
		DaemonInstalled:  launchctlLoaded(ctx, daemonLabel),
		DaemonRunning:    launchctlRunning(ctx, daemonLabel),
		RuntimeInstalled: launchctlLoaded(ctx, runtimeLabel),
		RuntimeRunning:   launchctlRunning(ctx, runtimeLabel),
		Platform:         "launchd",
	}, nil
}

// launchd helpers

func launchAgentsDir() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("get home directory: %w", err)
	}
	return filepath.Join(home, "Library", "LaunchAgents"), nil
}

func launchctlBootstrap(ctx context.Context, label, plistPath string) error {
	uid := fmt.Sprintf("gui/%d", os.Getuid())
	out, err := exec.CommandContext(ctx, "launchctl", "bootstrap", uid, plistPath).CombinedOutput()
	if err == nil {
		return nil
	}
	// already loaded — bootout and retry
	_ = launchctlBootout(ctx, label)
	out, err = exec.CommandContext(ctx, "launchctl", "bootstrap", uid, plistPath).CombinedOutput()
	if err != nil {
		return fmt.Errorf("launchctl bootstrap: %s: %w", strings.TrimSpace(string(out)), err)
	}
	return nil
}

func launchctlBootout(ctx context.Context, label string) error {
	uid := fmt.Sprintf("gui/%d", os.Getuid())
	target := uid + "/" + label
	out, err := exec.CommandContext(ctx, "launchctl", "bootout", target).CombinedOutput()
	if err != nil {
		return fmt.Errorf("launchctl bootout: %s: %w", strings.TrimSpace(string(out)), err)
	}
	return nil
}

func launchctlLoaded(_ context.Context, label string) bool {
	uid := fmt.Sprintf("gui/%d", os.Getuid())
	target := uid + "/" + label
	err := exec.Command("launchctl", "print", target).Run()
	return err == nil
}

func launchctlRunning(_ context.Context, label string) bool {
	uid := fmt.Sprintf("gui/%d", os.Getuid())
	target := uid + "/" + label
	out, err := exec.Command("launchctl", "print", target).CombinedOutput()
	if err != nil {
		return false
	}
	// "state = running" appears in launchctl print output
	return strings.Contains(string(out), "state = running")
}

// binary resolution

func resolveBinary(name string) (string, error) {
	if exePath, err := os.Executable(); err == nil {
		candidate := filepath.Join(filepath.Dir(exePath), name)
		if st, err := os.Stat(candidate); err == nil && !st.IsDir() {
			return candidate, nil
		}
	}
	if p, err := exec.LookPath(name); err == nil {
		return p, nil
	}
	return "", fmt.Errorf("%s not found in PATH or next to executable", name)
}

func resolveRuntimeBinary(ployzBin string) string {
	dir := filepath.Dir(ployzBin)
	candidate := filepath.Join(dir, "ployz-runtime")
	if st, err := os.Stat(candidate); err == nil && !st.IsDir() {
		return candidate
	}
	if p, err := exec.LookPath("ployz-runtime"); err == nil {
		return p
	}
	return ""
}

func runtimeBinArgs(runtimeBin, ployzBin, dataRoot string) []string {
	if runtimeBin != "" {
		return []string{runtimeBin, "--data-root", dataRoot}
	}
	return []string{ployzBin, "runtime", "run", "--data-root", dataRoot}
}

// plist templates

type plistData struct {
	Label     string
	Program   string
	Args      []string
	LogPath   string
	RunAtLoad bool
	KeepAlive bool
}

var daemonPlistTmpl = template.Must(template.New("daemon").Parse(plistXML))
var runtimePlistTmpl = template.Must(template.New("runtime").Parse(plistXML))

const plistXML = `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{{.Label}}</string>

  <key>ProgramArguments</key>
  <array>
    <string>{{.Program}}</string>
{{- range .Args}}
    <string>{{.}}</string>
{{- end}}
  </array>

  <key>RunAtLoad</key>
  <{{if .RunAtLoad}}true{{else}}false{{end}}/>

  <key>KeepAlive</key>
  <{{if .KeepAlive}}true{{else}}false{{end}}/>

  <key>StandardOutPath</key>
  <string>{{.LogPath}}</string>

  <key>StandardErrorPath</key>
  <string>{{.LogPath}}</string>
</dict>
</plist>
`

func renderPlist(tmpl *template.Template, data plistData) ([]byte, error) {
	var buf bytes.Buffer
	if err := tmpl.Execute(&buf, data); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}
