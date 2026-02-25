package configure

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"ployz/pkg/sdk/agent"
	"ployz/pkg/sdk/telemetry"

	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/codes"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.opentelemetry.io/otel/sdk/trace/tracetest"
	"go.opentelemetry.io/otel/trace"
)

type fakeDaemonService struct {
	installConfig agent.InstallConfig
	installErr    error
	status        agent.ServiceStatus
	statusErr     error
}

func (f *fakeDaemonService) Install(_ context.Context, cfg agent.InstallConfig) error {
	f.installConfig = cfg
	return f.installErr
}

func (f *fakeDaemonService) Uninstall(context.Context) error {
	return nil
}

func (f *fakeDaemonService) Status(context.Context) (agent.ServiceStatus, error) {
	return f.status, f.statusErr
}

type fakeHelperService struct {
	configureFn func(context.Context, HelperOptions) error
	status      HelperStatus
	statusErr   error
	configured  HelperOptions
}

func (f *fakeHelperService) Configure(ctx context.Context, opts HelperOptions) error {
	f.configured = opts
	if f.configureFn != nil {
		return f.configureFn(ctx, opts)
	}
	return nil
}

func (f *fakeHelperService) Status(context.Context) (HelperStatus, error) {
	return f.status, f.statusErr
}

func TestConfigureSuccess(t *testing.T) {
	tempDir := t.TempDir()
	dataRoot := filepath.Join(tempDir, "networks")
	tokenPath := filepath.Join(tempDir, "private", "helper.token")
	daemon := &fakeDaemonService{}
	helper := &fakeHelperService{configureFn: func(_ context.Context, opts HelperOptions) error {
		if err := os.MkdirAll(filepath.Dir(opts.TokenPath), 0o700); err != nil {
			return err
		}
		return os.WriteFile(opts.TokenPath, []byte("token\n"), 0o600)
	}}

	var waitReadyCalled bool
	svc := NewWithDependencies(Dependencies{
		DaemonService:      daemon,
		HelperService:      helper,
		EnsureDaemonUser:   func(context.Context, string) (int, int, error) { return 1000, 1000, nil },
		EnsureDataPaths:    func(string, int, int, string) error { return nil },
		EnsureDockerAccess: func(context.Context) error { return nil },
		WaitDaemonReady: func(context.Context, string, time.Duration) error {
			waitReadyCalled = true
			return nil
		},
		EnsureTokenOwner: func(string, int, int) error { return nil },
		GOOS:             "linux",
		GetEUID:          func() int { return 0 },
	})

	result, err := svc.Configure(context.Background(), Options{
		DataRoot:       dataRoot,
		SocketPath:     filepath.Join(tempDir, "ployzd.sock"),
		TUNSocketPath:  filepath.Join(tempDir, "tun.sock"),
		PrivSocketPath: filepath.Join(tempDir, "helper.sock"),
		TokenPath:      tokenPath,
		MTU:            1280,
	})
	if err != nil {
		t.Fatalf("Configure() error = %v", err)
	}

	if daemon.installConfig.DataRoot != dataRoot {
		t.Fatalf("daemon install data_root = %q, want %q", daemon.installConfig.DataRoot, dataRoot)
	}
	if daemon.installConfig.SocketPath == "" {
		t.Fatal("daemon install socket path should be set")
	}
	if !waitReadyCalled {
		t.Fatal("wait ready should be called")
	}
	if helper.configured.TokenPath != tokenPath {
		t.Fatalf("helper token path = %q, want %q", helper.configured.TokenPath, tokenPath)
	}
	if result.DaemonLogPath != filepath.Join(dataRoot, "ployzd.log") {
		t.Fatalf("daemon log path = %q", result.DaemonLogPath)
	}
}

func TestConfigureRequiresRoot(t *testing.T) {
	svc := NewWithDependencies(Dependencies{
		DaemonService:      &fakeDaemonService{},
		HelperService:      &fakeHelperService{},
		EnsureDaemonUser:   func(context.Context, string) (int, int, error) { return 0, 0, nil },
		EnsureDataPaths:    func(string, int, int, string) error { return nil },
		EnsureDockerAccess: func(context.Context) error { return nil },
		WaitDaemonReady:    func(context.Context, string, time.Duration) error { return nil },
		EnsureTokenOwner:   func(string, int, int) error { return nil },
		GOOS:               "linux",
		GetEUID:            func() int { return 1000 },
	})

	_, err := svc.Configure(context.Background(), Options{
		DataRoot:       "/tmp/ployz/networks",
		SocketPath:     "/tmp/ployz.sock",
		TUNSocketPath:  "/tmp/tun.sock",
		PrivSocketPath: "/tmp/helper.sock",
		TokenPath:      "/tmp/ployz/private/helper.token",
		MTU:            1280,
	})
	if err == nil {
		t.Fatal("Configure() error = nil, want root error")
	}
	if !strings.Contains(err.Error(), "requires root privileges") {
		t.Fatalf("Configure() error = %v, want root privileges message", err)
	}
}

func TestConfigureInvokesDataPathReconciliation(t *testing.T) {
	tempDir := t.TempDir()
	dataRoot := filepath.Join(tempDir, "networks")
	tokenPath := filepath.Join(tempDir, "private", "helper.token")
	helper := &fakeHelperService{configureFn: func(_ context.Context, opts HelperOptions) error {
		if err := os.MkdirAll(filepath.Dir(opts.TokenPath), 0o700); err != nil {
			return err
		}
		return os.WriteFile(opts.TokenPath, []byte("token\n"), 0o600)
	}}

	called := false
	var gotRoot string
	var gotUID int
	var gotGID int
	var gotGOOS string

	svc := NewWithDependencies(Dependencies{
		DaemonService:    &fakeDaemonService{},
		HelperService:    helper,
		EnsureDaemonUser: func(context.Context, string) (int, int, error) { return 1001, 2001, nil },
		EnsureDataPaths: func(dataRoot string, uid, gid int, goos string) error {
			called = true
			gotRoot = dataRoot
			gotUID = uid
			gotGID = gid
			gotGOOS = goos
			return nil
		},
		EnsureDockerAccess: func(context.Context) error { return nil },
		WaitDaemonReady:    func(context.Context, string, time.Duration) error { return nil },
		EnsureTokenOwner:   func(string, int, int) error { return nil },
		GOOS:               "darwin",
		GetEUID:            func() int { return 0 },
	})

	_, err := svc.Configure(context.Background(), Options{
		DataRoot:       dataRoot,
		SocketPath:     filepath.Join(tempDir, "ployzd.sock"),
		TUNSocketPath:  filepath.Join(tempDir, "tun.sock"),
		PrivSocketPath: filepath.Join(tempDir, "helper.sock"),
		TokenPath:      tokenPath,
		MTU:            1280,
	})
	if err != nil {
		t.Fatalf("Configure() error = %v", err)
	}
	if !called {
		t.Fatal("expected EnsureDataPaths to be called")
	}
	if gotRoot != dataRoot {
		t.Fatalf("EnsureDataPaths root = %q, want %q", gotRoot, dataRoot)
	}
	if gotUID != 1001 || gotGID != 2001 {
		t.Fatalf("EnsureDataPaths uid:gid = %d:%d, want 1001:2001", gotUID, gotGID)
	}
	if gotGOOS != "darwin" {
		t.Fatalf("EnsureDataPaths goos = %q, want darwin", gotGOOS)
	}
}

func TestConfigureRejectsTokenPathInsideDataRoot(t *testing.T) {
	tempDir := t.TempDir()
	dataRoot := filepath.Join(tempDir, "networks")

	svc := NewWithDependencies(Dependencies{
		DaemonService:      &fakeDaemonService{},
		HelperService:      &fakeHelperService{},
		EnsureDaemonUser:   func(context.Context, string) (int, int, error) { return 0, 0, nil },
		EnsureDataPaths:    func(string, int, int, string) error { return nil },
		EnsureDockerAccess: func(context.Context) error { return nil },
		WaitDaemonReady:    func(context.Context, string, time.Duration) error { return nil },
		EnsureTokenOwner:   func(string, int, int) error { return nil },
		GOOS:               "linux",
		GetEUID:            func() int { return 0 },
	})

	_, err := svc.Configure(context.Background(), Options{
		DataRoot:       dataRoot,
		SocketPath:     filepath.Join(tempDir, "ployzd.sock"),
		TUNSocketPath:  filepath.Join(tempDir, "tun.sock"),
		PrivSocketPath: filepath.Join(tempDir, "helper.sock"),
		TokenPath:      filepath.Join(dataRoot, "helper.token"),
		MTU:            1280,
	})
	if err == nil {
		t.Fatal("Configure() error = nil, want token path validation error")
	}
	if !strings.Contains(err.Error(), "outside data root") {
		t.Fatalf("Configure() error = %v, want outside data root message", err)
	}
}

func TestConfigureRejectsTokenPathInDataRootParent(t *testing.T) {
	tempDir := t.TempDir()
	dataRoot := filepath.Join(tempDir, "networks")

	svc := NewWithDependencies(Dependencies{
		DaemonService:      &fakeDaemonService{},
		HelperService:      &fakeHelperService{},
		EnsureDaemonUser:   func(context.Context, string) (int, int, error) { return 0, 0, nil },
		EnsureDataPaths:    func(string, int, int, string) error { return nil },
		EnsureDockerAccess: func(context.Context) error { return nil },
		WaitDaemonReady:    func(context.Context, string, time.Duration) error { return nil },
		EnsureTokenOwner:   func(string, int, int) error { return nil },
		GOOS:               "linux",
		GetEUID:            func() int { return 0 },
	})

	_, err := svc.Configure(context.Background(), Options{
		DataRoot:       dataRoot,
		SocketPath:     filepath.Join(tempDir, "ployzd.sock"),
		TUNSocketPath:  filepath.Join(tempDir, "tun.sock"),
		PrivSocketPath: filepath.Join(tempDir, "helper.sock"),
		TokenPath:      filepath.Join(tempDir, "helper.token"),
		MTU:            1280,
	})
	if err == nil {
		t.Fatal("Configure() error = nil, want token path validation error")
	}
	if !strings.Contains(err.Error(), "dedicated private directory") {
		t.Fatalf("Configure() error = %v, want dedicated private directory message", err)
	}
}

func TestConfigureEmitsTelemetryOnSuccess(t *testing.T) {
	tempDir := t.TempDir()
	dataRoot := filepath.Join(tempDir, "networks")
	tokenPath := filepath.Join(tempDir, "private", "helper.token")
	daemon := &fakeDaemonService{}
	helper := &fakeHelperService{configureFn: func(_ context.Context, opts HelperOptions) error {
		if err := os.MkdirAll(filepath.Dir(opts.TokenPath), 0o700); err != nil {
			return err
		}
		return os.WriteFile(opts.TokenPath, []byte("token\n"), 0o600)
	}}

	tracer, recorder := newTestTracer()
	svc := NewWithDependencies(Dependencies{
		DaemonService:      daemon,
		HelperService:      helper,
		Tracer:             tracer,
		EnsureDaemonUser:   func(context.Context, string) (int, int, error) { return 1000, 1000, nil },
		EnsureDataPaths:    func(string, int, int, string) error { return nil },
		EnsureDockerAccess: func(context.Context) error { return nil },
		WaitDaemonReady:    func(context.Context, string, time.Duration) error { return nil },
		EnsureTokenOwner:   func(string, int, int) error { return nil },
		GOOS:               "linux",
		GetEUID:            func() int { return 0 },
	})

	_, err := svc.Configure(context.Background(), Options{
		DataRoot:       dataRoot,
		SocketPath:     filepath.Join(tempDir, "ployzd.sock"),
		TUNSocketPath:  filepath.Join(tempDir, "tun.sock"),
		PrivSocketPath: filepath.Join(tempDir, "helper.sock"),
		TokenPath:      tokenPath,
		MTU:            1280,
		Tracer:         tracer,
	})
	if err != nil {
		t.Fatalf("Configure() error = %v", err)
	}

	spans := recorder.Ended()
	root := findSpanByName(spans, "configure")
	if root == nil {
		t.Fatal("missing root configure span")
	}
	if len(root.Events()) == 0 {
		t.Fatal("expected root plan event")
	}
	if root.Events()[0].Name != telemetry.PlanEventName {
		t.Fatalf("root event name = %q, want %q", root.Events()[0].Name, telemetry.PlanEventName)
	}
	if got := eventAttr(root.Events()[0].Attributes, telemetry.PlanVersionKey); got != telemetry.PlanVersion {
		t.Fatalf("plan version = %q, want %q", got, telemetry.PlanVersion)
	}

	for _, stepID := range []string{
		configureStepPreflight,
		configureStepDaemonUser,
		configureStepDataPaths,
		configureStepDockerAccess,
		configureStepHelper,
		configureStepTokenOwner,
		configureStepDaemonInstall,
		configureStepDaemonReady,
	} {
		if span := findSpanByName(spans, stepID); span == nil {
			t.Fatalf("missing step span %q", stepID)
		}
	}
}

func TestConfigureEmitsTelemetryFailureOnStepError(t *testing.T) {
	tempDir := t.TempDir()
	helperErr := errors.New("helper configure failed")

	tracer, recorder := newTestTracer()
	svc := NewWithDependencies(Dependencies{
		DaemonService:      &fakeDaemonService{},
		HelperService:      &fakeHelperService{configureFn: func(context.Context, HelperOptions) error { return helperErr }},
		Tracer:             tracer,
		EnsureDaemonUser:   func(context.Context, string) (int, int, error) { return 1000, 1000, nil },
		EnsureDataPaths:    func(string, int, int, string) error { return nil },
		EnsureDockerAccess: func(context.Context) error { return nil },
		WaitDaemonReady:    func(context.Context, string, time.Duration) error { return nil },
		EnsureTokenOwner:   func(string, int, int) error { return nil },
		GOOS:               "linux",
		GetEUID:            func() int { return 0 },
	})

	_, err := svc.Configure(context.Background(), Options{
		DataRoot:       filepath.Join(tempDir, "networks"),
		SocketPath:     filepath.Join(tempDir, "ployzd.sock"),
		TUNSocketPath:  filepath.Join(tempDir, "tun.sock"),
		PrivSocketPath: filepath.Join(tempDir, "helper.sock"),
		TokenPath:      filepath.Join(tempDir, "private", "helper.token"),
		MTU:            1280,
		Tracer:         tracer,
	})
	if err == nil {
		t.Fatal("Configure() error = nil, want helper failure")
	}

	spans := recorder.Ended()
	helperSpan := findSpanByName(spans, configureStepHelper)
	if helperSpan == nil {
		t.Fatal("missing helper step span")
	}
	if helperSpan.Status().Code != codes.Error {
		t.Fatalf("helper status code = %v, want %v", helperSpan.Status().Code, codes.Error)
	}
	if !strings.Contains(helperSpan.Status().Description, helperErr.Error()) {
		t.Fatalf("helper status description = %q, want %q", helperSpan.Status().Description, helperErr.Error())
	}
}

func TestStatusIncludesCheckErrors(t *testing.T) {
	daemonErr := errors.New("daemon status failed")
	helperErr := errors.New("helper status failed")
	svc := NewWithDependencies(Dependencies{
		DaemonService: &fakeDaemonService{
			status:    agent.ServiceStatus{DaemonInstalled: true, DaemonRunning: false, Platform: "launchd"},
			statusErr: daemonErr,
		},
		HelperService: &fakeHelperService{
			status:    HelperStatus{Installed: false, Running: false},
			statusErr: helperErr,
		},
		HealthCheck:      func(context.Context, string) error { return fmt.Errorf("not ready") },
		CheckSocketReady: func(string) bool { return false },
		CheckFileExists:  func(string) bool { return true },
		GOOS:             "darwin",
	})

	st, err := svc.Status(context.Background(), StatusOptions{
		DataRoot:       "/tmp/ployz",
		SocketPath:     "/tmp/ployz.sock",
		PrivSocketPath: "/tmp/helper.sock",
		TokenPath:      "/tmp/helper.token",
	})
	if err != nil {
		t.Fatalf("Status() error = %v", err)
	}
	if st.DaemonReady {
		t.Fatal("daemon ready should be false")
	}
	if st.DaemonStatusError == "" || st.HelperStatusError == "" {
		t.Fatalf("expected status errors, got daemon=%q helper=%q", st.DaemonStatusError, st.HelperStatusError)
	}
	if st.Platform != "darwin" {
		t.Fatalf("platform = %q, want darwin fallback", st.Platform)
	}
	if !st.HelperTokenPresent {
		t.Fatal("helper token present should be true")
	}
}

func newTestTracer() (trace.Tracer, *tracetest.SpanRecorder) {
	recorder := tracetest.NewSpanRecorder()
	provider := sdktrace.NewTracerProvider(sdktrace.WithSpanProcessor(recorder))
	return provider.Tracer("configure-test"), recorder
}

func findSpanByName(spans []sdktrace.ReadOnlySpan, name string) sdktrace.ReadOnlySpan {
	for _, span := range spans {
		if span.Name() == name {
			return span
		}
	}
	return nil
}

func eventAttr(attrs []attribute.KeyValue, key string) string {
	for _, attr := range attrs {
		if string(attr.Key) == key {
			return attr.Value.AsString()
		}
	}
	return ""
}
