//go:build darwin

package process

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/netip"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"time"
)

const (
	readinessPollInterval = 500 * time.Millisecond
	readinessProbeTimeout = 2 * time.Second
	corrosionReadyTimeout = 30 * time.Second
	pidStopGrace          = 8 * time.Second
	pidStopForce          = 2 * time.Second
	pidPollInterval       = 200 * time.Millisecond
	maxTailLogBytes       = 32 * 1024
	corrosionBinaryName   = "corrosion"
	corrosionBinaryEnv    = "PLOYZ_CORROSION_BIN"
	storeDBFileName       = "store.db"
)

var (
	corrosionLookPath   = exec.LookPath
	corrosionStat       = os.Stat
	corrosionExecutable = os.Executable
	corrosionGetenv     = os.Getenv
)

type RuntimeConfig struct {
	Name       string
	ConfigPath string
	DataDir    string
	GossipAddr netip.AddrPort
	APIAddr    netip.AddrPort
	APIToken   string
}

func Start(ctx context.Context, cfg RuntimeConfig) error {
	name := strings.TrimSpace(cfg.Name)
	if name == "" {
		return fmt.Errorf("corrosion process name is required")
	}
	if strings.TrimSpace(cfg.ConfigPath) == "" {
		return fmt.Errorf("corrosion config path is required")
	}
	if strings.TrimSpace(cfg.DataDir) == "" {
		return fmt.Errorf("corrosion data dir is required")
	}

	log := slog.With("component", "corrosion-runtime", "mode", "process", "name", name)
	log.Info("starting")

	if err := validateGossipBindAddr(cfg.GossipAddr); err != nil {
		return fmt.Errorf("validate corrosion gossip bind address: %w", err)
	}

	corrosionBin, err := resolveCorrosionBinary()
	if err != nil {
		return fmt.Errorf("resolve corrosion binary: %w", err)
	}
	log.Debug("resolved corrosion binary", "path", corrosionBin)

	pidPath := pidFilePath(name)
	if err := stopFromPIDFile(pidPath, pidStopGrace); err != nil {
		return fmt.Errorf("stop stale corrosion process: %w", err)
	}

	if err := os.MkdirAll(cfg.DataDir, 0o755); err != nil {
		return fmt.Errorf("ensure corrosion data dir: %w", err)
	}
	dbPath := filepath.Join(cfg.DataDir, storeDBFileName)
	dbExisted, err := prepareCorrosionDataDir(cfg.DataDir)
	if err != nil {
		return fmt.Errorf("prepare corrosion data dir: %w", err)
	}

	logPath := filepath.Join(cfg.DataDir, "corrosion.log")
	logFile, err := os.OpenFile(logPath, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		return fmt.Errorf("open corrosion log file: %w", err)
	}

	cmd := exec.Command(corrosionBin, "agent", "-c", cfg.ConfigPath)
	cmd.Stdout = logFile
	cmd.Stderr = logFile
	if err := cmd.Start(); err != nil {
		_ = logFile.Close()
		return fmt.Errorf("start corrosion process: %w", err)
	}

	pid := cmd.Process.Pid
	if err := writePIDFile(pidPath, pid); err != nil {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
		_ = logFile.Close()
		return fmt.Errorf("write corrosion pid file: %w", err)
	}

	exitCh := make(chan error, 1)
	go reapCorrosionProcess(cmd, logFile, pidPath, pid, log, exitCh)

	if err := waitReady(ctx, name, pid, cfg.APIAddr, cfg.APIToken, logPath, corrosionReadyTimeout, exitCh); err != nil {
		_ = stopFromPIDFile(pidPath, pidStopGrace)
		if dbExisted && strings.Contains(strings.ToLower(err.Error()), "segmentation fault") {
			return fmt.Errorf("%s (existing corrosion database %s may be incompatible; move aside store.db, store.db-shm, and store.db-wal, then retry `ployz init --force`)", err.Error(), dbPath)
		}
		return err
	}

	if err := applySchema(ctx, cfg.APIAddr, cfg.APIToken); err != nil {
		return err
	}

	log.Info("api ready")
	return nil
}

func resolveCorrosionBinary() (string, error) {
	if explicit := strings.TrimSpace(corrosionGetenv(corrosionBinaryEnv)); explicit != "" {
		if ok, err := isExecutableFile(explicit); err != nil {
			return "", fmt.Errorf("check %s %q: %w", corrosionBinaryEnv, explicit, err)
		} else if !ok {
			return "", fmt.Errorf("%s=%q is not an executable file", corrosionBinaryEnv, explicit)
		}
		return explicit, nil
	}

	candidates := defaultCorrosionBinaryCandidates()
	for _, candidate := range candidates {
		ok, err := isExecutableFile(candidate)
		if err != nil {
			continue
		}
		if ok {
			return candidate, nil
		}
	}

	if path, err := corrosionLookPath(corrosionBinaryName); err == nil {
		return path, nil
	}

	return "", fmt.Errorf("exec: %q: executable file not found in PATH (install corrosion with `just install` or set %s)", corrosionBinaryName, corrosionBinaryEnv)
}

func defaultCorrosionBinaryCandidates() []string {
	candidates := make([]string, 0, 4)
	if exePath, err := corrosionExecutable(); err == nil {
		candidates = append(candidates, filepath.Join(filepath.Dir(exePath), corrosionBinaryName))
	}
	candidates = append(candidates,
		"/usr/local/bin/corrosion",
		"/opt/homebrew/bin/corrosion",
	)
	return candidates
}

func isExecutableFile(path string) (bool, error) {
	if strings.TrimSpace(path) == "" {
		return false, nil
	}
	st, err := corrosionStat(path)
	if err != nil {
		return false, err
	}
	if st.IsDir() {
		return false, nil
	}
	return st.Mode()&0o111 != 0, nil
}

func Stop(ctx context.Context, name string) error {
	trimmed := strings.TrimSpace(name)
	if trimmed == "" {
		return fmt.Errorf("corrosion process name is required")
	}
	log := slog.With("component", "corrosion-runtime", "mode", "process", "name", trimmed)
	log.Info("stopping")

	pidPath := pidFilePath(trimmed)
	if err := stopFromPIDFileContext(ctx, pidPath, pidStopGrace); err != nil {
		return fmt.Errorf("stop corrosion process: %w", err)
	}
	return nil
}

func APIReady(ctx context.Context, apiAddr netip.AddrPort, apiToken string) bool {
	httpClient := &http.Client{Timeout: readinessProbeTimeout}
	body := []byte(`{"query":"SELECT 1","params":[]}`)
	url := "http://" + apiAddr.String() + "/v1/queries"

	req, err := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewReader(body))
	if err != nil {
		return false
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Accept", "application/json")
	if strings.TrimSpace(apiToken) != "" {
		req.Header.Set("Authorization", "Bearer "+apiToken)
	}

	resp, err := httpClient.Do(req)
	if err != nil {
		return false
	}
	defer func() { _ = resp.Body.Close() }()
	if resp.StatusCode != http.StatusOK {
		return false
	}

	var event struct {
		Error *string `json:"error"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&event); err != nil {
		return false
	}
	return event.Error == nil || strings.TrimSpace(*event.Error) == ""
}

func prepareCorrosionDataDir(dataDir string) (bool, error) {
	dbPath := filepath.Join(dataDir, storeDBFileName)
	dbExisted := false
	if st, err := corrosionStat(dbPath); err == nil {
		dbExisted = true
		if st.IsDir() {
			return false, fmt.Errorf("sqlite path is a directory: %s", dbPath)
		}
		f, openErr := os.OpenFile(dbPath, os.O_RDWR, 0)
		if openErr != nil {
			if errors.Is(openErr, os.ErrPermission) {
				return false, fmt.Errorf("sqlite database %s is not writable by daemon user; run `sudo ployz configure` to reconcile ownership", dbPath)
			}
			return false, fmt.Errorf("open sqlite database %s: %w", dbPath, openErr)
		}
		_ = f.Close()
	} else if !errors.Is(err, os.ErrNotExist) {
		return false, fmt.Errorf("stat sqlite database %s: %w", dbPath, err)
	}

	for _, sidecar := range []string{dbPath + "-wal", dbPath + "-shm", dbPath + "-journal"} {
		if err := os.Remove(sidecar); err != nil && !errors.Is(err, os.ErrNotExist) {
			return false, fmt.Errorf("remove sqlite sidecar %s: %w", sidecar, err)
		}
	}

	return dbExisted, nil
}

func reapCorrosionProcess(cmd *exec.Cmd, logFile *os.File, pidPath string, pid int, log *slog.Logger, exitCh chan<- error) {
	err := cmd.Wait()
	_ = logFile.Close()
	removePIDIfMatches(pidPath, pid)
	select {
	case exitCh <- err:
	default:
	}
	if err == nil {
		log.Info("process exited", "pid", pid)
		return
	}
	log.Warn("process exited with error", "pid", pid, "err", err)
}

func waitReady(ctx context.Context, name string, pid int, apiAddr netip.AddrPort, apiToken, logPath string, timeout time.Duration, exitCh <-chan error) error {
	log := slog.With("component", "corrosion-runtime", "mode", "process", "name", name, "api_addr", apiAddr.String())
	ticker := time.NewTicker(readinessPollInterval)
	defer ticker.Stop()
	timer := time.NewTimer(timeout)
	defer timer.Stop()

	var lastErr string
	for {
		select {
		case exitErr := <-exitCh:
			msg := fmt.Sprintf("corrosion process exited before readiness (pid %d)", pid)
			if exitErr != nil {
				msg += ": " + exitErr.Error()
			}
			if logs := tailLog(logPath, maxTailLogBytes); logs != "" {
				msg += "\n" + logs
			}
			return fmt.Errorf("%s", msg)
		default:
		}

		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-timer.C:
			msg := "corrosion process not ready after " + timeout.String()
			if strings.TrimSpace(lastErr) != "" {
				msg += ": " + lastErr
			}
			if logs := tailLog(logPath, maxTailLogBytes); logs != "" {
				msg += "\n" + logs
			}
			log.Warn("readiness timeout", "detail", msg)
			return fmt.Errorf("%s", msg)
		case <-ticker.C:
			running, runErr := isProcessRunning(pid)
			if runErr == nil && !running {
				msg := fmt.Sprintf("corrosion process exited before readiness (pid %d)", pid)
				if logs := tailLog(logPath, maxTailLogBytes); logs != "" {
					msg += "\n" + logs
				}
				return fmt.Errorf("%s", msg)
			}
			probeCtx, cancel := context.WithTimeout(ctx, readinessProbeTimeout)
			ready := APIReady(probeCtx, apiAddr, apiToken)
			cancel()
			if ready {
				return nil
			}
			lastErr = "query endpoint unavailable"
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
	if strings.TrimSpace(apiToken) != "" {
		req.Header.Set("Authorization", "Bearer "+apiToken)
	}

	resp, err := (&http.Client{Timeout: 10 * time.Second}).Do(req)
	if err != nil {
		return fmt.Errorf("apply schema: %w", err)
	}
	defer func() { _ = resp.Body.Close() }()

	if resp.StatusCode != http.StatusOK {
		data, _ := io.ReadAll(resp.Body)
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

func validateGossipBindAddr(gossipAddr netip.AddrPort) error {
	localAddrs, err := localInterfaceAddrs()
	if err != nil {
		return fmt.Errorf("list host interface addresses: %w", err)
	}
	return validateGossipBindAddrWithLocalAddrs(gossipAddr, localAddrs)
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

func pidFilePath(name string) string {
	safe := sanitizeName(name)
	return filepath.Join(os.TempDir(), "ployz-corrosion-"+safe+".pid")
}

func sanitizeName(name string) string {
	trimmed := strings.TrimSpace(name)
	if trimmed == "" {
		return "default"
	}
	var b strings.Builder
	b.Grow(len(trimmed))
	for _, r := range trimmed {
		switch {
		case r >= 'a' && r <= 'z':
			b.WriteRune(r)
		case r >= 'A' && r <= 'Z':
			b.WriteRune(r)
		case r >= '0' && r <= '9':
			b.WriteRune(r)
		case r == '-', r == '_', r == '.':
			b.WriteRune(r)
		default:
			b.WriteRune('_')
		}
	}
	out := b.String()
	if out == "" {
		return "default"
	}
	return out
}

func writePIDFile(path string, pid int) error {
	if pid <= 0 {
		return fmt.Errorf("invalid pid %d", pid)
	}
	return os.WriteFile(path, []byte(strconv.Itoa(pid)+"\n"), 0o600)
}

func readPIDFile(path string) (int, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return 0, err
	}
	pid, err := strconv.Atoi(strings.TrimSpace(string(data)))
	if err != nil {
		return 0, fmt.Errorf("parse pid file %s: %w", path, err)
	}
	if pid <= 0 {
		return 0, fmt.Errorf("invalid pid %d in %s", pid, path)
	}
	return pid, nil
}

func stopFromPIDFile(path string, timeout time.Duration) error {
	return stopFromPIDFileContext(context.Background(), path, timeout)
}

func stopFromPIDFileContext(ctx context.Context, path string, timeout time.Duration) error {
	pid, err := readPIDFile(path)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil
		}
		return err
	}

	running, err := isProcessRunning(pid)
	if err != nil {
		_ = os.Remove(path)
		return nil
	}
	if !running {
		_ = os.Remove(path)
		return nil
	}

	isCorrosion, err := isCorrosionProcess(pid)
	if err != nil {
		return err
	}
	if !isCorrosion {
		_ = os.Remove(path)
		return nil
	}

	if err := signalProcess(pid, syscall.SIGTERM); err != nil {
		if errors.Is(err, syscall.ESRCH) || errors.Is(err, os.ErrProcessDone) {
			_ = os.Remove(path)
			return nil
		}
		return err
	}

	if err := waitProcessExit(ctx, pid, timeout); err == nil {
		_ = os.Remove(path)
		return nil
	}

	if killErr := signalProcess(pid, syscall.SIGKILL); killErr != nil && !errors.Is(killErr, syscall.ESRCH) && !errors.Is(killErr, os.ErrProcessDone) {
		return killErr
	}
	if err := waitProcessExit(ctx, pid, pidStopForce); err != nil {
		return err
	}
	_ = os.Remove(path)
	return nil
}

func signalProcess(pid int, sig syscall.Signal) error {
	proc, err := os.FindProcess(pid)
	if err != nil {
		return err
	}
	return proc.Signal(sig)
}

func isProcessRunning(pid int) (bool, error) {
	err := signalProcess(pid, syscall.Signal(0))
	if err == nil {
		return true, nil
	}
	if errors.Is(err, syscall.ESRCH) || errors.Is(err, os.ErrProcessDone) {
		return false, nil
	}
	return false, err
}

func waitProcessExit(ctx context.Context, pid int, timeout time.Duration) error {
	ticker := time.NewTicker(pidPollInterval)
	defer ticker.Stop()
	timer := time.NewTimer(timeout)
	defer timer.Stop()

	for {
		running, err := isProcessRunning(pid)
		if err != nil {
			if errors.Is(err, syscall.ESRCH) || errors.Is(err, os.ErrProcessDone) {
				return nil
			}
			return err
		}
		if !running {
			return nil
		}

		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-timer.C:
			return fmt.Errorf("process %d did not exit within %s", pid, timeout)
		case <-ticker.C:
		}
	}
}

func isCorrosionProcess(pid int) (bool, error) {
	out, err := exec.Command("ps", "-o", "command=", "-p", strconv.Itoa(pid)).CombinedOutput()
	if err != nil {
		if len(bytes.TrimSpace(out)) == 0 {
			return false, nil
		}
		return false, fmt.Errorf("inspect process %d command: %w", pid, err)
	}
	line := strings.ToLower(strings.TrimSpace(string(out)))
	if line == "" {
		return false, nil
	}
	return strings.Contains(line, "corrosion") && strings.Contains(line, "agent"), nil
}

func removePIDIfMatches(path string, pid int) {
	existing, err := readPIDFile(path)
	if err != nil {
		return
	}
	if existing != pid {
		return
	}
	_ = os.Remove(path)
}

func tailLog(path string, maxBytes int) string {
	if maxBytes <= 0 {
		return ""
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return ""
	}
	if len(data) <= maxBytes {
		return strings.TrimSpace(string(data))
	}
	return strings.TrimSpace(string(data[len(data)-maxBytes:]))
}
