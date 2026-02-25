package configure

import (
	"context"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"time"

	"ployz/internal/adapter/wireguard"
	"ployz/pkg/sdk/agent"
	"ployz/pkg/sdk/client"
	"ployz/pkg/sdk/defaults"
	"ployz/pkg/sdk/telemetry"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/trace"
)

const (
	defaultTUNMTU   = 1280
	daemonReadyWait = 15 * time.Second

	configureStepPreflight     = "preflight"
	configureStepDaemonUser    = "ensure_daemon_user"
	configureStepDataPaths     = "reconcile_data_paths"
	configureStepDockerAccess  = "ensure_docker_access"
	configureStepHelper        = "configure_helper"
	configureStepTokenOwner    = "set_token_owner"
	configureStepDaemonInstall = "install_daemon"
	configureStepDaemonReady   = "wait_daemon_ready"

	dataRootModeDarwin = 0o775
	dataRootModeUnix   = 0o755
	privateDirMode     = 0o700
	tokenFileMode      = 0o600
)

type Options struct {
	DataRoot       string
	SocketPath     string
	TUNSocketPath  string
	PrivSocketPath string
	TokenPath      string
	MTU            int
	Tracer         trace.Tracer
}

type StatusOptions struct {
	DataRoot       string
	SocketPath     string
	PrivSocketPath string
	TokenPath      string
}

type Result struct {
	SocketPath     string
	DataRoot       string
	DaemonLogPath  string
	PrivSocketPath string
	TokenPath      string
	TUNSocketPath  string
	MTU            int
}

type StatusResult struct {
	Platform           string
	DaemonInstalled    bool
	DaemonRunning      bool
	DaemonReady        bool
	HelperInstalled    bool
	HelperRunning      bool
	HelperSocketReady  bool
	HelperTokenPresent bool
	SocketPath         string
	PrivSocketPath     string
	TokenPath          string
	DataRoot           string
	DaemonLogPath      string
	DaemonStatusError  string
	DaemonReadyError   string
	HelperStatusError  string
}

type Dependencies struct {
	DaemonService      agent.PlatformService
	HelperService      HelperService
	Tracer             trace.Tracer
	EnsureDaemonUser   func(ctx context.Context, dataRoot string) (int, int, error)
	EnsureDataPaths    func(dataRoot string, uid, gid int, goos string) error
	EnsureDockerAccess func(ctx context.Context) error
	WaitDaemonReady    func(ctx context.Context, socketPath string, timeout time.Duration) error
	HealthCheck        func(ctx context.Context, socketPath string) error
	EnsureTokenOwner   func(tokenPath string, uid, gid int) error
	CheckSocketReady   func(socketPath string) bool
	CheckFileExists    func(path string) bool
	GetEUID            func() int
	GOOS               string
}

type Service struct {
	daemonService      agent.PlatformService
	helperService      HelperService
	tracer             trace.Tracer
	ensureDaemonUser   func(ctx context.Context, dataRoot string) (int, int, error)
	ensureDataPaths    func(dataRoot string, uid, gid int, goos string) error
	ensureDockerAccess func(ctx context.Context) error
	waitDaemonReady    func(ctx context.Context, socketPath string, timeout time.Duration) error
	healthCheck        func(ctx context.Context, socketPath string) error
	ensureTokenOwner   func(tokenPath string, uid, gid int) error
	checkSocketReady   func(socketPath string) bool
	checkFileExists    func(path string) bool
	geteuid            func() int
	goos               string
}

func New() *Service {
	return NewWithDependencies(Dependencies{})
}

func NewWithDependencies(deps Dependencies) *Service {
	if deps.DaemonService == nil {
		deps.DaemonService = agent.NewPlatformService()
	}
	if deps.HelperService == nil {
		deps.HelperService = newPlatformHelperService()
	}
	if deps.Tracer == nil {
		deps.Tracer = otel.Tracer("ployz/sdk/configure")
	}
	if deps.EnsureDaemonUser == nil {
		deps.EnsureDaemonUser = agent.EnsureDaemonUser
	}
	if deps.EnsureDataPaths == nil {
		deps.EnsureDataPaths = ensureDataPathPermissions
	}
	if deps.EnsureDockerAccess == nil {
		deps.EnsureDockerAccess = defaultEnsureDockerAccess
	}
	if deps.WaitDaemonReady == nil {
		deps.WaitDaemonReady = agent.WaitReady
	}
	if deps.HealthCheck == nil {
		deps.HealthCheck = agent.HealthCheck
	}
	if deps.EnsureTokenOwner == nil {
		deps.EnsureTokenOwner = ensureTokenOwnership
	}
	if deps.CheckSocketReady == nil {
		deps.CheckSocketReady = checkUnixSocketReady
	}
	if deps.CheckFileExists == nil {
		deps.CheckFileExists = fileExists
	}
	if deps.GetEUID == nil {
		deps.GetEUID = os.Geteuid
	}
	if strings.TrimSpace(deps.GOOS) == "" {
		deps.GOOS = runtime.GOOS
	}

	return &Service{
		daemonService:      deps.DaemonService,
		helperService:      deps.HelperService,
		tracer:             deps.Tracer,
		ensureDaemonUser:   deps.EnsureDaemonUser,
		ensureDataPaths:    deps.EnsureDataPaths,
		ensureDockerAccess: deps.EnsureDockerAccess,
		waitDaemonReady:    deps.WaitDaemonReady,
		healthCheck:        deps.HealthCheck,
		ensureTokenOwner:   deps.EnsureTokenOwner,
		checkSocketReady:   deps.CheckSocketReady,
		checkFileExists:    deps.CheckFileExists,
		geteuid:            deps.GetEUID,
		goos:               deps.GOOS,
	}
}

func (s *Service) Configure(ctx context.Context, opts Options) (Result, error) {
	if requiresRoot(s.goos) && s.geteuid() != 0 {
		return Result{}, fmt.Errorf("configure requires root privileges; run `sudo ployz configure`")
	}

	resolved, err := normalizeConfigureOptions(opts)
	if err != nil {
		return Result{}, err
	}

	tracer := opts.Tracer
	if tracer == nil {
		tracer = s.tracer
	}
	op, err := telemetry.EmitPlan(ctx, tracer, "configure", telemetry.Plan{Steps: []telemetry.PlannedStep{
		{ID: configureStepPreflight, Title: "checking privileges"},
		{ID: configureStepDaemonUser, Title: "ensuring daemon user"},
		{ID: configureStepDataPaths, Title: "reconciling data path permissions"},
		{ID: configureStepDockerAccess, Title: "ensuring Docker access"},
		{ID: configureStepHelper, Title: "configuring privileged helper"},
		{ID: configureStepTokenOwner, Title: "setting helper token ownership"},
		{ID: configureStepDaemonInstall, Title: "installing daemon service"},
		{ID: configureStepDaemonReady, Title: "waiting for daemon readiness"},
	}})
	if err != nil {
		return Result{}, err
	}

	var opErr error
	defer func() {
		op.End(opErr)
	}()

	uid := 0
	gid := 0
	steps := []struct {
		id string
		fn func(context.Context) error
	}{
		{
			id: configureStepPreflight,
			fn: func(context.Context) error {
				if requiresRoot(s.goos) && s.geteuid() != 0 {
					return fmt.Errorf("configure requires root privileges; run `sudo ployz configure`")
				}
				return nil
			},
		},
		{
			id: configureStepDaemonUser,
			fn: func(stepCtx context.Context) error {
				resolvedUID, resolvedGID, stepErr := s.ensureDaemonUser(stepCtx, resolved.DataRoot)
				if stepErr != nil {
					return fmt.Errorf("ensure daemon user: %w", stepErr)
				}
				uid = resolvedUID
				gid = resolvedGID
				return nil
			},
		},
		{
			id: configureStepDataPaths,
			fn: func(context.Context) error {
				if stepErr := s.ensureDataPaths(resolved.DataRoot, uid, gid, s.goos); stepErr != nil {
					return fmt.Errorf("reconcile data path permissions: %w", stepErr)
				}
				return nil
			},
		},
		{
			id: configureStepDockerAccess,
			fn: func(stepCtx context.Context) error {
				if stepErr := s.ensureDockerAccess(stepCtx); stepErr != nil {
					return fmt.Errorf("ensure docker access: %w", stepErr)
				}
				return nil
			},
		},
		{
			id: configureStepHelper,
			fn: func(stepCtx context.Context) error {
				return s.helperService.Configure(stepCtx, HelperOptions{
					TUNSocketPath:  resolved.TUNSocketPath,
					PrivSocketPath: resolved.PrivSocketPath,
					TokenPath:      resolved.TokenPath,
					MTU:            resolved.MTU,
				})
			},
		},
		{
			id: configureStepTokenOwner,
			fn: func(context.Context) error {
				return s.ensureTokenOwner(resolved.TokenPath, uid, gid)
			},
		},
		{
			id: configureStepDaemonInstall,
			fn: func(stepCtx context.Context) error {
				if stepErr := s.daemonService.Install(stepCtx, agent.InstallConfig{
					DataRoot:   resolved.DataRoot,
					SocketPath: resolved.SocketPath,
				}); stepErr != nil {
					return fmt.Errorf("install daemon service: %w", stepErr)
				}
				return nil
			},
		},
		{
			id: configureStepDaemonReady,
			fn: func(stepCtx context.Context) error {
				if stepErr := s.waitDaemonReady(stepCtx, resolved.SocketPath, daemonReadyWait); stepErr != nil {
					return fmt.Errorf("%w (check daemon log: %s)", stepErr, agent.DaemonLogPath(resolved.DataRoot))
				}
				return nil
			},
		},
	}

	for _, step := range steps {
		opErr = op.RunStep(op.Context(), step.id, step.fn)
		if opErr != nil {
			return Result{}, opErr
		}
	}

	result := Result{
		SocketPath:     resolved.SocketPath,
		DataRoot:       resolved.DataRoot,
		DaemonLogPath:  agent.DaemonLogPath(resolved.DataRoot),
		PrivSocketPath: resolved.PrivSocketPath,
		TokenPath:      resolved.TokenPath,
		TUNSocketPath:  resolved.TUNSocketPath,
		MTU:            resolved.MTU,
	}
	return result, nil
}

func (s *Service) Status(ctx context.Context, opts StatusOptions) (StatusResult, error) {
	resolved, err := normalizeStatusOptions(opts)
	if err != nil {
		return StatusResult{}, err
	}

	daemonStatus, daemonErr := s.daemonService.Status(ctx)
	daemonReadyErr := s.healthCheck(ctx, resolved.SocketPath)
	daemonReady := daemonReadyErr == nil

	helperStatus, helperErr := s.helperService.Status(ctx)

	platform := s.goos
	if daemonErr == nil && strings.TrimSpace(daemonStatus.Platform) != "" {
		platform = daemonStatus.Platform
	}

	out := StatusResult{
		Platform:           platform,
		DaemonInstalled:    daemonStatus.DaemonInstalled,
		DaemonRunning:      daemonStatus.DaemonRunning,
		DaemonReady:        daemonReady,
		HelperInstalled:    helperStatus.Installed,
		HelperRunning:      helperStatus.Running,
		HelperSocketReady:  s.checkSocketReady(resolved.PrivSocketPath),
		HelperTokenPresent: s.checkFileExists(resolved.TokenPath),
		SocketPath:         resolved.SocketPath,
		PrivSocketPath:     resolved.PrivSocketPath,
		TokenPath:          resolved.TokenPath,
		DataRoot:           resolved.DataRoot,
		DaemonLogPath:      agent.DaemonLogPath(resolved.DataRoot),
	}
	if daemonErr != nil {
		out.DaemonStatusError = daemonErr.Error()
	}
	if daemonReadyErr != nil {
		out.DaemonReadyError = daemonReadyErr.Error()
	}
	if helperErr != nil {
		out.HelperStatusError = helperErr.Error()
	}
	return out, nil
}

func normalizeConfigureOptions(opts Options) (Options, error) {
	if strings.TrimSpace(opts.DataRoot) == "" {
		opts.DataRoot = defaults.DataRoot()
	}
	if strings.TrimSpace(opts.SocketPath) == "" {
		opts.SocketPath = client.DefaultSocketPath()
	}
	if strings.TrimSpace(opts.TUNSocketPath) == "" {
		opts.TUNSocketPath = wireguard.DefaultTUNSocketPath()
	}
	if strings.TrimSpace(opts.PrivSocketPath) == "" {
		opts.PrivSocketPath = wireguard.DefaultPrivilegedSocketPath()
	}
	if strings.TrimSpace(opts.TokenPath) == "" {
		opts.TokenPath = wireguard.DefaultPrivilegedTokenPath()
	}
	if opts.MTU == 0 {
		opts.MTU = defaultTUNMTU
	}
	if opts.MTU <= 0 {
		return Options{}, fmt.Errorf("invalid mtu %d", opts.MTU)
	}

	var err error
	opts.PrivSocketPath, err = cleanNonDotPath(opts.PrivSocketPath, "invalid privileged helper socket path")
	if err != nil {
		return Options{}, err
	}
	opts.TokenPath, err = cleanNonDotPath(opts.TokenPath, "invalid helper token path")
	if err != nil {
		return Options{}, err
	}
	opts.SocketPath, err = cleanNonDotPath(opts.SocketPath, "invalid daemon socket path")
	if err != nil {
		return Options{}, err
	}
	opts.DataRoot, err = cleanNonDotPath(opts.DataRoot, "invalid data root path")
	if err != nil {
		return Options{}, err
	}
	if err := validateTokenPath(opts.TokenPath, opts.DataRoot); err != nil {
		return Options{}, err
	}
	opts.TUNSocketPath = strings.TrimSpace(opts.TUNSocketPath)
	if opts.TUNSocketPath == "" {
		return Options{}, fmt.Errorf("invalid tun socket path")
	}

	return opts, nil
}

func normalizeStatusOptions(opts StatusOptions) (StatusOptions, error) {
	if strings.TrimSpace(opts.DataRoot) == "" {
		opts.DataRoot = defaults.DataRoot()
	}
	if strings.TrimSpace(opts.SocketPath) == "" {
		opts.SocketPath = client.DefaultSocketPath()
	}
	if strings.TrimSpace(opts.PrivSocketPath) == "" {
		opts.PrivSocketPath = wireguard.DefaultPrivilegedSocketPath()
	}
	if strings.TrimSpace(opts.TokenPath) == "" {
		opts.TokenPath = wireguard.DefaultPrivilegedTokenPath()
	}

	var err error
	opts.DataRoot, err = cleanNonDotPath(opts.DataRoot, "invalid data root path")
	if err != nil {
		return StatusOptions{}, err
	}
	opts.SocketPath, err = cleanNonDotPath(opts.SocketPath, "invalid daemon socket path")
	if err != nil {
		return StatusOptions{}, err
	}
	opts.PrivSocketPath, err = cleanNonDotPath(opts.PrivSocketPath, "invalid privileged helper socket path")
	if err != nil {
		return StatusOptions{}, err
	}
	opts.TokenPath, err = cleanNonDotPath(opts.TokenPath, "invalid helper token path")
	if err != nil {
		return StatusOptions{}, err
	}

	return opts, nil
}

func requiresRoot(goos string) bool {
	return goos == "darwin" || goos == "linux"
}

func ensureTokenOwnership(tokenPath string, uid, gid int) error {
	path, err := cleanNonDotPath(tokenPath, "invalid token path")
	if err != nil {
		return err
	}

	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, privateDirMode); err != nil {
		return fmt.Errorf("create helper token directory: %w", err)
	}
	if err := os.Chown(dir, uid, gid); err != nil {
		return fmt.Errorf("set helper token directory owner: %w", err)
	}
	if err := os.Chmod(dir, privateDirMode); err != nil {
		return fmt.Errorf("set helper token directory permissions: %w", err)
	}
	if err := os.Chown(path, uid, gid); err != nil {
		return fmt.Errorf("set helper token owner: %w", err)
	}
	if err := os.Chmod(path, tokenFileMode); err != nil {
		return fmt.Errorf("set helper token permissions: %w", err)
	}
	return nil
}

func ensureDataPathPermissions(dataRoot string, uid, gid int, goos string) error {
	root, err := cleanNonDotPath(dataRoot, "invalid data root path")
	if err != nil {
		return err
	}

	rootDir := filepath.Dir(root)
	privateDir := filepath.Join(rootDir, "private")

	if err := ensureOwnedDir(rootDir, uid, gid, dataRootModeUnix); err != nil {
		return fmt.Errorf("prepare data root parent %q: %w", rootDir, err)
	}

	dataMode := dataRootModeForOS(goos)
	if err := ensureOwnedDir(root, uid, gid, dataMode); err != nil {
		return fmt.Errorf("prepare data root %q: %w", root, err)
	}
	if err := reconcileDataTree(root, uid, gid, dataMode); err != nil {
		return fmt.Errorf("reconcile data root tree %q: %w", root, err)
	}

	if err := ensureOwnedDir(privateDir, uid, gid, privateDirMode); err != nil {
		return fmt.Errorf("prepare private root %q: %w", privateDir, err)
	}

	return nil
}

func ensureOwnedDir(path string, uid, gid int, mode os.FileMode) error {
	if err := os.MkdirAll(path, mode); err != nil {
		return err
	}
	if err := os.Chown(path, uid, gid); err != nil {
		return err
	}
	if err := os.Chmod(path, mode); err != nil {
		return err
	}
	return nil
}

func reconcileDataTree(root string, uid, gid int, dirMode os.FileMode) error {
	return filepath.WalkDir(root, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if d.Type()&os.ModeSymlink != 0 {
			return nil
		}
		if err := os.Chown(path, uid, gid); err != nil {
			return err
		}
		if d.IsDir() {
			if err := os.Chmod(path, dirMode); err != nil {
				return err
			}
		}
		return nil
	})
}

func dataRootModeForOS(goos string) os.FileMode {
	if goos == "darwin" {
		return dataRootModeDarwin
	}
	return dataRootModeUnix
}

func validateTokenPath(tokenPath, dataRoot string) error {
	if isPathWithin(tokenPath, dataRoot) {
		return fmt.Errorf("helper token path %q must be outside data root %q", tokenPath, dataRoot)
	}

	dataRootParent := filepath.Dir(filepath.Clean(dataRoot))
	tokenParent := filepath.Dir(filepath.Clean(tokenPath))
	if tokenParent == dataRootParent {
		return fmt.Errorf("helper token path %q must be inside a dedicated private directory (recommended: %s)", tokenPath, wireguard.DefaultPrivilegedTokenPath())
	}

	return nil
}

func isPathWithin(candidate, base string) bool {
	cleanCandidate := filepath.Clean(candidate)
	cleanBase := filepath.Clean(base)
	rel, err := filepath.Rel(cleanBase, cleanCandidate)
	if err != nil {
		return false
	}
	if rel == "." {
		return true
	}
	parentPrefix := ".." + string(filepath.Separator)
	return rel != ".." && !strings.HasPrefix(rel, parentPrefix)
}

func cleanNonDotPath(rawPath, invalidErrMsg string) (string, error) {
	path := filepath.Clean(strings.TrimSpace(rawPath))
	if path == "." {
		return "", errors.New(invalidErrMsg)
	}
	return path, nil
}

func checkUnixSocketReady(socketPath string) bool {
	if _, err := os.Stat(socketPath); err != nil {
		return false
	}
	conn, err := dialUnixSocket(socketPath)
	if err != nil {
		return false
	}
	_ = conn.Close()
	return true
}

func fileExists(path string) bool {
	_, err := os.Stat(path)
	if err == nil {
		return true
	}
	return errors.Is(err, fs.ErrPermission)
}
