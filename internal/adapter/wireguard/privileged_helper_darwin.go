//go:build darwin

package wireguard

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"
)

func RunPrivilegedHelper(ctx context.Context, socketPath, token string) error {
	path := strings.TrimSpace(socketPath)
	tok := strings.TrimSpace(token)
	if path == "" {
		return fmt.Errorf("privileged helper socket path is required")
	}
	if tok == "" {
		return fmt.Errorf("privileged helper token is required")
	}

	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return fmt.Errorf("create privileged helper socket dir: %w", err)
	}
	if err := os.Remove(path); err != nil && !errors.Is(err, os.ErrNotExist) {
		return fmt.Errorf("remove stale privileged helper socket: %w", err)
	}

	ln, err := net.Listen("unix", path)
	if err != nil {
		return fmt.Errorf("listen privileged helper socket: %w", err)
	}
	pidPath := path + ".pid"
	if err := os.WriteFile(pidPath, []byte(strconv.Itoa(os.Getpid())), 0o600); err != nil {
		_ = ln.Close()
		_ = os.Remove(path)
		return fmt.Errorf("write privileged helper pid file: %w", err)
	}
	if err := os.Chmod(path, 0o666); err != nil {
		_ = ln.Close()
		_ = os.Remove(path)
		_ = os.Remove(pidPath)
		return fmt.Errorf("set privileged helper socket permissions: %w", err)
	}

	log := slog.With("component", "wireguard-priv-helper", "socket", path)
	log.Info("privileged helper started")

	go func() {
		<-ctx.Done()
		_ = ln.Close()
	}()

	defer func() {
		_ = ln.Close()
		_ = os.Remove(path)
		_ = os.Remove(pidPath)
		log.Info("privileged helper stopped")
	}()

	for {
		conn, err := ln.Accept()
		if err != nil {
			if ctx.Err() != nil {
				return nil
			}
			if ne, ok := err.(net.Error); ok && ne.Temporary() {
				continue
			}
			return fmt.Errorf("accept privileged helper request: %w", err)
		}
		go servePrivilegedConn(ctx, conn, tok)
	}
}

var (
	privilegedMu     sync.RWMutex
	privilegedBroker *privilegedBrokerConfig
)

type privilegedBrokerConfig struct {
	socketPath string
	token      string
}

type privilegedRequest struct {
	Token     string   `json:"token"`
	Name      string   `json:"name"`
	Args      []string `json:"args"`
	TimeoutMS int      `json:"timeout_ms,omitempty"`
}

type privilegedResponse struct {
	Output string `json:"output,omitempty"`
	Error  string `json:"error,omitempty"`
}

const configureHint = "run `sudo ployz configure`"

func installPrivilegedBroker(socketPath, token string) error {
	path := strings.TrimSpace(socketPath)
	tok := strings.TrimSpace(token)
	if path == "" {
		return fmt.Errorf("privileged helper socket path is required")
	}
	if tok == "" {
		return fmt.Errorf("privileged helper token is required")
	}

	privilegedMu.Lock()
	privilegedBroker = &privilegedBrokerConfig{socketPath: path, token: tok}
	privilegedMu.Unlock()
	return nil
}

func runPrivilegedCommand(ctx context.Context, name string, args ...string) ([]byte, error) {
	if os.Geteuid() == 0 {
		return exec.CommandContext(ctx, name, args...).CombinedOutput()
	}

	cfg, err := privilegedBrokerSnapshot()
	if err != nil {
		return nil, err
	}

	conn, err := (&net.Dialer{}).DialContext(ctx, "unix", cfg.socketPath)
	if err != nil {
		if shouldSuggestConfigure(err) {
			return nil, configureRequiredError("privileged helper is unavailable", err)
		}
		return nil, fmt.Errorf("connect privileged helper: %w", err)
	}
	defer conn.Close()

	timeoutMS := 30000
	if deadline, ok := ctx.Deadline(); ok {
		remaining := time.Until(deadline)
		if remaining <= 0 {
			return nil, context.DeadlineExceeded
		}
		timeoutMS = int(remaining.Milliseconds())
	}

	enc := json.NewEncoder(conn)
	if err := enc.Encode(privilegedRequest{
		Token:     cfg.token,
		Name:      strings.TrimSpace(name),
		Args:      append([]string(nil), args...),
		TimeoutMS: timeoutMS,
	}); err != nil {
		if shouldSuggestConfigure(err) {
			return nil, configureRequiredError("privileged helper is unavailable", err)
		}
		return nil, fmt.Errorf("send privileged request: %w", err)
	}

	resp := privilegedResponse{}
	dec := json.NewDecoder(conn)
	if err := dec.Decode(&resp); err != nil {
		if shouldSuggestConfigure(err) {
			return nil, configureRequiredError("privileged helper is unavailable", err)
		}
		return nil, fmt.Errorf("read privileged response: %w", err)
	}
	out := []byte(resp.Output)
	if strings.TrimSpace(resp.Error) != "" {
		if strings.TrimSpace(resp.Error) == "unauthorized" {
			return out, configureRequiredError("privileged helper credentials are stale", nil)
		}
		return out, errors.New(resp.Error)
	}
	return out, nil
}

func privilegedBrokerSnapshot() (privilegedBrokerConfig, error) {
	privilegedMu.RLock()
	state := privilegedBroker
	privilegedMu.RUnlock()
	if state == nil {
		return privilegedBrokerConfig{}, configureRequiredError("privileged helper is not configured", nil)
	}
	return *state, nil
}

func shouldSuggestConfigure(err error) bool {
	if err == nil {
		return false
	}
	return errors.Is(err, os.ErrNotExist) ||
		errors.Is(err, syscall.ENOENT) ||
		errors.Is(err, syscall.ECONNREFUSED) ||
		errors.Is(err, syscall.ECONNRESET) ||
		errors.Is(err, syscall.EPIPE)
}

func configureRequiredError(message string, err error) error {
	if err != nil {
		return fmt.Errorf("%s: %w; %s", message, err, configureHint)
	}
	return fmt.Errorf("%s; %s", message, configureHint)
}

func servePrivilegedConn(parent context.Context, conn net.Conn, token string) {
	defer conn.Close()

	req := privilegedRequest{}
	dec := json.NewDecoder(conn)
	if err := dec.Decode(&req); err != nil {
		_ = json.NewEncoder(conn).Encode(privilegedResponse{Error: fmt.Sprintf("decode request: %v", err)})
		return
	}
	if req.Token != token {
		_ = json.NewEncoder(conn).Encode(privilegedResponse{Error: "unauthorized"})
		return
	}
	if req.TimeoutMS <= 0 {
		req.TimeoutMS = 30000
	}

	name := strings.TrimSpace(req.Name)
	if err := validatePrivilegedCommand(name, req.Args); err != nil {
		_ = json.NewEncoder(conn).Encode(privilegedResponse{Error: err.Error()})
		return
	}

	execCtx, cancel := context.WithTimeout(parent, time.Duration(req.TimeoutMS)*time.Millisecond)
	defer cancel()

	out, err := exec.CommandContext(execCtx, name, req.Args...).CombinedOutput()
	resp := privilegedResponse{Output: string(out)}
	if err != nil {
		resp.Error = err.Error()
	}
	_ = json.NewEncoder(conn).Encode(resp)
}

func validatePrivilegedCommand(name string, args []string) error {
	if name == "" {
		return fmt.Errorf("command name is required")
	}
	switch name {
	case "ifconfig", "route":
		// allowed
	default:
		return fmt.Errorf("command %q is not allowed", name)
	}

	for _, arg := range args {
		if strings.TrimSpace(arg) == "" {
			return fmt.Errorf("command arguments must be non-empty")
		}
	}

	return nil
}
