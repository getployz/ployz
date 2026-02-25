//go:build darwin

package agent

import (
	"bytes"
	"context"
	"fmt"
	"os"
	"os/exec"
	"os/user"
	"path/filepath"
	"strconv"
	"strings"
	"text/template"
	"time"
)

const daemonLabel = "com.ployz.ployzd"
const daemonUser = "ployzd"
const daemonUserShell = "/usr/bin/false"
const daemonUserHome = "/var/empty"
const daemonUserRealName = "Ployz Daemon"
const daemonPrimaryGroupID = 20
const daemonUIDStart = 550

type darwinService struct{}

func NewPlatformService() PlatformService {
	return &darwinService{}
}

func (d *darwinService) Install(ctx context.Context, cfg InstallConfig) error {
	if os.Geteuid() != 0 {
		return fmt.Errorf("agent install requires root — run with sudo")
	}
	if err := removeStaleSocket(cfg.SocketPath); err != nil {
		return fmt.Errorf("remove stale daemon socket: %w", err)
	}

	uid, gid, err := ensureDarwinDaemonUser(ctx)
	if err != nil {
		return err
	}

	daemonsDir := launchDaemonsDir()
	if err := os.MkdirAll(daemonsDir, 0o755); err != nil {
		return fmt.Errorf("create LaunchDaemons dir: %w", err)
	}
	if err := reconcileDaemonDataPaths(cfg.DataRoot, uid, gid, "darwin"); err != nil {
		return fmt.Errorf("prepare data paths: %w", err)
	}

	logPath := DaemonLogPath(cfg.DataRoot)
	logFile, err := os.OpenFile(logPath, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		return fmt.Errorf("open daemon log file: %w", err)
	}
	if err := logFile.Close(); err != nil {
		return fmt.Errorf("close daemon log file: %w", err)
	}
	if err := os.Chown(logPath, uid, gid); err != nil {
		return fmt.Errorf("set daemon log owner: %w", err)
	}

	ployzBin, err := resolveBinary("ployz")
	if err != nil {
		return fmt.Errorf("resolve ployz binary: %w", err)
	}

	daemonPlist, err := renderPlist(daemonPlistTmpl, plistData{
		Label:     daemonLabel,
		Program:   ployzBin,
		Args:      []string{"daemon", "run", "--socket", cfg.SocketPath, "--data-root", cfg.DataRoot},
		LogPath:   DaemonLogPath(cfg.DataRoot),
		UserName:  daemonUser,
		RunAtLoad: true,
		KeepAlive: true,
	})
	if err != nil {
		return fmt.Errorf("render daemon plist: %w", err)
	}

	daemonPath := filepath.Join(daemonsDir, daemonLabel+".plist")

	_ = launchctlBootout(ctx, daemonLabel)
	waitForUnloaded(ctx, daemonLabel)

	if err := os.WriteFile(daemonPath, daemonPlist, 0o644); err != nil {
		return fmt.Errorf("write daemon plist: %w", err)
	}

	if err := launchctlBootstrap(ctx, daemonLabel, daemonPath); err != nil {
		return fmt.Errorf("bootstrap daemon: %w", err)
	}

	return nil
}

func (d *darwinService) Uninstall(ctx context.Context) error {
	_ = launchctlBootout(ctx, daemonLabel)
	os.Remove(filepath.Join(launchDaemonsDir(), daemonLabel+".plist"))

	return nil
}

func (d *darwinService) Status(ctx context.Context) (ServiceStatus, error) {
	return ServiceStatus{
		DaemonInstalled: launchctlLoaded(ctx, daemonLabel),
		DaemonRunning:   launchctlRunning(ctx, daemonLabel),
		Platform:        "launchd",
	}, nil
}

func launchDaemonsDir() string {
	return "/Library/LaunchDaemons"
}

func launchctlBootstrap(ctx context.Context, label, plistPath string) error {
	out, err := exec.CommandContext(ctx, "launchctl", "bootstrap", "system", plistPath).CombinedOutput()
	if err == nil {
		return nil
	}
	_ = launchctlBootout(ctx, label)
	waitForUnloaded(ctx, label)
	out, err = exec.CommandContext(ctx, "launchctl", "bootstrap", "system", plistPath).CombinedOutput()
	if err != nil {
		return fmt.Errorf("launchctl bootstrap: %s: %w", strings.TrimSpace(string(out)), err)
	}
	return nil
}

// waitForUnloaded polls until launchd reports the service is gone.
// This prevents bootstrap from racing against a still-unloading service.
func waitForUnloaded(ctx context.Context, label string) {
	const maxWait = 5 * time.Second
	const pollInterval = 100 * time.Millisecond
	deadline := time.Now().Add(maxWait)
	for time.Now().Before(deadline) {
		if !launchctlLoaded(ctx, label) {
			return
		}
		select {
		case <-ctx.Done():
			return
		case <-time.After(pollInterval):
		}
	}
}

func launchctlBootout(ctx context.Context, label string) error {
	target := "system/" + label
	out, err := exec.CommandContext(ctx, "launchctl", "bootout", target).CombinedOutput()
	if err != nil {
		return fmt.Errorf("launchctl bootout: %s: %w", strings.TrimSpace(string(out)), err)
	}
	return nil
}

func launchctlLoaded(_ context.Context, label string) bool {
	target := "system/" + label
	err := exec.Command("launchctl", "print", target).Run()
	return err == nil
}

func launchctlRunning(_ context.Context, label string) bool {
	target := "system/" + label
	out, err := exec.Command("launchctl", "print", target).CombinedOutput()
	if err != nil {
		return false
	}
	return strings.Contains(string(out), "state = running")
}

func resolveBinary(name string) (string, error) {
	if exePath, err := os.Executable(); err == nil && !isGoRunExecutablePath(exePath) {
		candidate := filepath.Join(filepath.Dir(exePath), name)
		if st, statErr := os.Stat(candidate); statErr == nil && !st.IsDir() {
			return candidate, nil
		}
	}
	if p, err := exec.LookPath(name); err == nil {
		return p, nil
	}
	defaultInstalled := filepath.Join("/usr/local/bin", name)
	if st, err := os.Stat(defaultInstalled); err == nil && !st.IsDir() {
		return defaultInstalled, nil
	}
	if exePath, err := os.Executable(); err == nil && isGoRunExecutablePath(exePath) {
		return "", fmt.Errorf("%s not found in PATH and current executable is a temporary go run binary (%s); run `just install` and retry", name, exePath)
	}
	return "", fmt.Errorf("%s not found in PATH or next to executable", name)
}

func isGoRunExecutablePath(path string) bool {
	trimmed := strings.TrimSpace(path)
	if trimmed == "" {
		return false
	}
	return strings.Contains(filepath.Clean(trimmed), string(filepath.Separator)+"go-build")
}

type plistData struct {
	Label     string
	Program   string
	Args      []string
	LogPath   string
	UserName  string
	RunAtLoad bool
	KeepAlive bool
}

var daemonPlistTmpl = template.Must(template.New("daemon").Parse(plistXML))

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

{{- if .UserName}}
  <key>UserName</key>
  <string>{{.UserName}}</string>
{{- end}}

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

func EnsureDaemonUser(ctx context.Context, _ string) (int, int, error) {
	return ensureDarwinDaemonUser(ctx)
}

func ensureDarwinDaemonUser(ctx context.Context) (int, int, error) {
	if uid, gid, err := lookupUserIDs(daemonUser); err == nil {
		return uid, gid, nil
	}

	uid, err := nextAvailableDarwinUID(ctx, daemonUIDStart)
	if err != nil {
		return 0, 0, err
	}

	userPath := "/Users/" + daemonUser
	createSteps := [][]string{
		{".", "-create", userPath},
		{".", "-create", userPath, "UserShell", daemonUserShell},
		{".", "-create", userPath, "RealName", daemonUserRealName},
		{".", "-create", userPath, "UniqueID", strconv.Itoa(uid)},
		{".", "-create", userPath, "PrimaryGroupID", strconv.Itoa(daemonPrimaryGroupID)},
		{".", "-create", userPath, "NFSHomeDirectory", daemonUserHome},
		{".", "-create", userPath, "IsHidden", "1"},
		{".", "-create", userPath, "Password", "*"},
	}
	for _, step := range createSteps {
		out, err := exec.CommandContext(ctx, "dscl", step...).CombinedOutput()
		if err != nil {
			if strings.Contains(string(out), "eDSRecordAlreadyExists") {
				continue
			}
			return 0, 0, fmt.Errorf("create daemon user: dscl %s: %s: %w", strings.Join(step, " "), strings.TrimSpace(string(out)), err)
		}
	}

	resolvedUID, resolvedGID, err := lookupUserIDs(daemonUser)
	if err != nil {
		return 0, 0, fmt.Errorf("lookup daemon user after create: %w", err)
	}
	return resolvedUID, resolvedGID, nil
}

func nextAvailableDarwinUID(ctx context.Context, start int) (int, error) {
	out, err := exec.CommandContext(ctx, "dscl", ".", "-list", "/Users", "UniqueID").CombinedOutput()
	if err != nil {
		return 0, fmt.Errorf("list existing users: %s: %w", strings.TrimSpace(string(out)), err)
	}
	used := make(map[int]struct{})
	lines := strings.Split(strings.TrimSpace(string(out)), "\n")
	for _, line := range lines {
		fields := strings.Fields(line)
		if len(fields) != 2 {
			continue
		}
		uid, parseErr := strconv.Atoi(fields[1])
		if parseErr != nil {
			continue
		}
		used[uid] = struct{}{}
	}
	for uid := start; uid < 65535; uid++ {
		if _, ok := used[uid]; !ok {
			return uid, nil
		}
	}
	return 0, fmt.Errorf("no free uid available from %d", start)
}

func lookupUserIDs(username string) (int, int, error) {
	u, err := user.Lookup(username)
	if err != nil {
		return 0, 0, err
	}
	uid, err := strconv.Atoi(u.Uid)
	if err != nil {
		return 0, 0, fmt.Errorf("parse uid %q: %w", u.Uid, err)
	}
	gid, err := strconv.Atoi(u.Gid)
	if err != nil {
		return 0, 0, fmt.Errorf("parse gid %q: %w", u.Gid, err)
	}
	return uid, gid, nil
}
