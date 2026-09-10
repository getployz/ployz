// ployz-tailcat is a private transport adapter. Stdout is exclusively RPC bytes.
package main

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log"
	"net"
	"os"
	"os/signal"
	"path/filepath"
	"reflect"
	"strings"
	"syscall"
	"time"

	"github.com/tailscale/tailcat"
	"tailscale.com/envknob"
	"tailscale.com/types/logger"
)

const (
	rpcPort         = 7443
	rpcSocket       = "/run/ployz/ployz.sock"
	defaultState    = "/var/lib/ployz/tailcat/state.json"
	capabilityLimit = 16 * 1024
)

var version = "dev"

var errCapability = errors.New("invalid Tailcat capability")

type endpointState struct {
	Key        *tailcat.PrivateKey `json:"key"`
	Capability tailcat.Addr        `json:"capability"`
}

func main() {
	// ponytail: direct large RPCs are unqualified on Cloud; enable UDP only after qualification.
	envknob.Setenv("TS_DEBUG_ALWAYS_USE_DERP", "true")
	// Upstream debug paths also use the process logger; never print capabilities.
	log.SetOutput(io.Discard)
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()
	if err := run(ctx, os.Args[1:]); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run(ctx context.Context, args []string) error {
	if len(args) == 1 && (args[0] == "version" || args[0] == "--version") {
		fmt.Fprintln(os.Stdout, version)
		return nil
	}
	if len(args) == 1 && args[0] == "successor" {
		return successorCapability(os.Stdin, os.Stdout)
	}
	if len(args) == 1 && (args[0] == "rotate" || args[0] == "validate-rotation") {
		return rotateCapability(defaultState, os.Stdin, args[0] == "rotate")
	}
	if len(args) == 1 && args[0] == "connect" {
		return connect(ctx)
	}
	if len(args) == 1 && args[0] == "export" {
		return exportCapability(defaultState, os.Stdout)
	}
	if len(args) == 1 && args[0] == "serve" {
		return serve(ctx, defaultState)
	}
	if len(args) == 3 && args[0] == "serve" && args[1] == "--state" {
		return serve(ctx, args[2])
	}
	return errors.New("usage: ployz-tailcat connect | export | successor | validate-rotation | rotate | serve [--state PATH]")
}

// Export only the ready capability; never generate or replace endpoint identity.
func exportCapability(path string, output io.Writer) error {
	state, err := readState(path)
	if err != nil {
		return err
	}
	if _, err := readCapability(bufio.NewReader(strings.NewReader(string(state.Capability) + "\n"))); err != nil {
		return errors.New("Tailcat endpoint has no ready capability")
	}
	_, err = fmt.Fprintln(output, state.Capability)
	return err
}

func readCapability(r *bufio.Reader) (tailcat.Addr, error) {
	line, err := r.ReadSlice('\n')
	if err != nil || len(line) > capabilityLimit || len(line) < 2 {
		return "", errCapability
	}
	addr := tailcat.Addr(line[:len(line)-1])
	ci, err := tailcat.ParseAddr(addr)
	if err != nil || ci.PresharedKey.IsZero() || ci.ServerPublic.IsZero() || ci.ServerDiscoPublic.IsZero() {
		return "", errCapability
	}
	return addr, nil
}

func connect(ctx context.Context) error {
	stop := context.AfterFunc(ctx, func() { os.Stdin.Close() })
	defer stop()
	input := bufio.NewReaderSize(os.Stdin, capabilityLimit+1)
	addr, err := readCapability(input)
	if err != nil {
		return err
	}
	client := &tailcat.Client{Server: addr, Logf: logger.Discard}
	defer client.Close()
	dialCtx, cancel := context.WithTimeout(ctx, 15*time.Second)
	conn, err := client.DialTCPPort(dialCtx, rpcPort)
	cancel()
	if err != nil {
		return errors.New("Tailcat connection failed")
	}
	defer conn.Close()
	// Closing stdin releases the upload goroutine when the remote ends first.
	defer os.Stdin.Close()
	if err := relay(ctx, conn, input, os.Stdout); err != nil {
		return err
	}
	drainCtx, drainCancel := context.WithTimeout(ctx, 5*time.Second)
	defer drainCancel()
	if err := client.DrainTCP(drainCtx); err != nil {
		return errors.New("Tailcat connection drain failed")
	}
	return nil
}

func relay(ctx context.Context, conn net.Conn, input io.Reader, output io.Writer) error {
	upload := make(chan error, 1)
	download := make(chan error, 1)
	go func() {
		_, err := io.Copy(conn, input)
		if err == nil {
			if half, ok := conn.(interface{ CloseWrite() error }); ok {
				err = half.CloseWrite()
			} else {
				err = errors.New("missing half-close")
			}
		}
		upload <- err
	}()
	go func() { _, err := io.Copy(output, conn); download <- err }()
	for {
		select {
		case <-ctx.Done():
			conn.Close()
			return errors.New("Tailcat connection canceled")
		case err := <-upload:
			if err != nil {
				conn.Close()
				return errors.New("Tailcat upload failed")
			}
			upload = nil
		case err := <-download:
			if err != nil {
				conn.Close()
				return errors.New("Tailcat download failed")
			}
			return nil
		}
	}
}

func readState(path string) (*endpointState, error) {
	info, err := os.Lstat(path)
	if err != nil {
		return nil, err
	}
	if !info.Mode().IsRegular() || info.Mode().Perm()&0077 != 0 || info.Size() > 64*1024 {
		return nil, errors.New("state must be a private regular file")
	}
	dir, err := os.Stat(filepath.Dir(path))
	if err != nil {
		return nil, err
	}
	if dir.Mode().Perm()&0077 != 0 {
		return nil, errors.New("state directory must be private")
	}
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	var state endpointState
	decoder := json.NewDecoder(io.LimitReader(f, 64*1024))
	if err := decoder.Decode(&state); err != nil {
		return nil, errors.New("invalid Tailcat state")
	}
	if decoder.Decode(new(any)) != io.EOF {
		return nil, errors.New("invalid Tailcat state")
	}
	if state.Key == nil || state.Key.Private.IsZero() || state.Key.Public.PresharedKey.IsZero() {
		return nil, errors.New("invalid Tailcat state")
	}
	return &state, nil
}

func writeState(path string, state *endpointState) error {
	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0700); err != nil {
		return err
	}
	info, err := os.Stat(dir)
	if err != nil {
		return err
	}
	if info.Mode().Perm()&0077 != 0 {
		return errors.New("state directory must be private")
	}
	data, err := json.Marshal(state)
	if err != nil {
		return err
	}
	f, err := os.CreateTemp(dir, ".tailcat-*")
	if err != nil {
		return err
	}
	defer os.Remove(f.Name())
	// A root-run rotation must retain the service account's ownership.
	if existing, err := os.Stat(path); err == nil {
		owner := existing.Sys().(*syscall.Stat_t)
		if err := f.Chown(int(owner.Uid), int(owner.Gid)); err != nil {
			f.Close()
			return err
		}
	} else if !errors.Is(err, os.ErrNotExist) {
		f.Close()
		return err
	}
	if _, err := f.Write(data); err != nil {
		f.Close()
		return err
	}
	if err := f.Sync(); err != nil {
		f.Close()
		return err
	}
	if err := f.Close(); err != nil {
		return err
	}
	if err := os.Rename(f.Name(), path); err != nil {
		return err
	}
	d, err := os.Open(dir)
	if err != nil {
		return err
	}
	defer d.Close()
	return d.Sync()
}

func serve(ctx context.Context, path string) error {
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	unlock, err := lockState(path)
	if err != nil {
		return errors.New("cannot lock Tailcat state")
	}
	defer unlock()
	state, err := readState(path)
	if errors.Is(err, os.ErrNotExist) {
		state = &endpointState{Key: tailcat.NewPrivateKey()}
		// Save identity before exposing an endpoint; restart must never silently rotate it.
		if err = writeState(path, state); err != nil {
			return errors.New("cannot persist Tailcat identity")
		}
	} else if err != nil {
		return errors.New("cannot read Tailcat identity")
	}
	server := &tailcat.Server{
		Key: state.Key.Private, PresharedKey: state.Key.Public.PresharedKey,
		Logf: logger.Discard, MaxClients: 128, ClientIdleTimeout: 30 * time.Second,
		OnTCP: func(port uint16) func(net.Conn) {
			if port != rpcPort {
				return nil
			}
			return func(remote net.Conn) {
				local, err := net.DialTimeout("unix", rpcSocket, 5*time.Second)
				if err != nil {
					remote.Close()
					return
				}
				proxyUntilCanceled(ctx, remote, local)
			}
		},
	}
	// Preserve the selected bootstrap relay, as well as keys, across restarts.
	if len(state.Key.Public.Region) > 0 {
		server.Region = state.Key.Public.Region[0]
	} else {
		server.RegionID = state.Key.Public.RegionID
	}
	if err := server.Start(); err != nil {
		server.Close()
		return errors.New("Tailcat endpoint startup failed")
	}
	defer func() {
		cancel()
		// Give the userspace TCP stack time to transmit connection closure before exit.
		drain, stop := context.WithTimeout(context.Background(), 2*time.Second)
		defer stop()
		_ = server.DrainTCP(drain)
		server.Close()
	}()
	state.Capability = server.TailcatAddr()
	state.Key.Public, err = tailcat.ParseAddr(state.Capability)
	if err != nil {
		return errors.New("Tailcat endpoint address failed")
	}
	if err := writeState(path, state); err != nil {
		return errors.New("cannot persist Tailcat endpoint")
	}
	if socket := os.Getenv("NOTIFY_SOCKET"); socket != "" {
		conn, err := net.Dial("unixgram", socket)
		if err != nil {
			return errors.New("cannot notify endpoint readiness")
		}
		_, err = conn.Write([]byte("READY=1"))
		conn.Close()
		if err != nil {
			return errors.New("cannot notify endpoint readiness")
		}
	}
	unlock()
	<-ctx.Done()
	return nil
}

// Lock the directory inode, avoiding lock-file ownership changes across root and
// the service account. Startup holds it until its last state write and READY.
func lockState(path string) (func(), error) {
	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		return nil, err
	}
	dir, err := os.Open(filepath.Dir(path))
	if err != nil {
		return nil, err
	}
	if err := syscall.Flock(int(dir.Fd()), syscall.LOCK_EX); err != nil {
		dir.Close()
		return nil, err
	}
	closed := false
	return func() {
		if !closed {
			closed = true
			dir.Close()
		}
	}, nil
}

func successorCapability(input io.Reader, output io.Writer) error {
	addr, err := readCapability(bufio.NewReaderSize(input, capabilityLimit+1))
	if err != nil {
		return err
	}
	ci, _ := tailcat.ParseAddr(addr)
	previous := ci.PresharedKey
	for ci.PresharedKey == previous || ci.PresharedKey.IsZero() {
		ci.PresharedKey = tailcat.NewPresharedKey()
	}
	_, err = fmt.Fprintln(output, ci.Addr())
	return err
}

func rotateCapability(path string, input io.Reader, persist bool) error {
	reader := bufio.NewReaderSize(input, capabilityLimit+1)
	expected, err := readCapability(reader)
	if err != nil {
		return err
	}
	successor, err := readCapability(reader)
	if err != nil {
		return err
	}
	oldInfo, _ := tailcat.ParseAddr(expected)
	nextInfo, _ := tailcat.ParseAddr(successor)
	immutable := nextInfo
	immutable.PresharedKey = oldInfo.PresharedKey
	if nextInfo.PresharedKey == oldInfo.PresharedKey || !reflect.DeepEqual(oldInfo, immutable) {
		return errors.New("invalid Tailcat removal successor")
	}
	unlock, err := lockState(path)
	if err != nil {
		return errors.New("cannot lock Tailcat state")
	}
	defer unlock()
	state, err := readState(path)
	if err != nil {
		return errors.New("cannot read Tailcat identity")
	}
	current, err := tailcat.ParseAddr(state.Capability)
	if err != nil || (!reflect.DeepEqual(current, oldInfo) && !reflect.DeepEqual(current, nextInfo)) {
		return errors.New("stale Tailcat removal")
	}
	if !reflect.DeepEqual(state.Key.Public, current) {
		return errors.New("inconsistent Tailcat state")
	}
	if persist {
		state.Capability = successor
		state.Key.Public = nextInfo
		if err := writeState(path, state); err != nil {
			return errors.New("cannot persist Tailcat successor")
		}
	}
	return nil
}

func proxyUntilCanceled(ctx context.Context, remote, local net.Conn) {
	stop := context.AfterFunc(ctx, func() {
		remote.Close()
		local.Close()
	})
	defer stop()
	tailcat.ProxyConns(remote, local)
}
