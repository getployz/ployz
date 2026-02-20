package client

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"net"
	"os/exec"
	"strings"
	"sync"
	"time"
)

const remotePloyzdPath = "/usr/local/bin/ployzd"

type SSHOptions struct {
	Port       int
	KeyPath    string
	SocketPath string
}

func NewSSH(target string, opts SSHOptions) (*Client, error) {
	target = strings.TrimSpace(target)
	if opts.SocketPath == "" {
		opts.SocketPath = DefaultSocketPath()
	}

	return NewWithDialer(func(ctx context.Context, _ string) (net.Conn, error) {
		return dialSSH(ctx, target, opts)
	})
}

func dialSSH(ctx context.Context, target string, opts SSHOptions) (net.Conn, error) {
	if target == "" {
		return nil, fmt.Errorf("ssh target is required")
	}

	args := []string{"-T", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=accept-new"}
	if opts.Port > 0 {
		args = append(args, "-p", fmt.Sprintf("%d", opts.Port))
	}
	if strings.TrimSpace(opts.KeyPath) != "" {
		args = append(args, "-i", opts.KeyPath)
	}
	args = append(args, target)
	args = append(args, dialStdioRemoteArgs(target, opts.SocketPath)...)

	if err := ctx.Err(); err != nil {
		return nil, err
	}
	cmd := exec.Command("ssh", args...)
	stderr := &bytes.Buffer{}
	cmd.Stderr = stderr

	stdin, err := cmd.StdinPipe()
	if err != nil {
		return nil, fmt.Errorf("open ssh stdin pipe: %w", err)
	}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		_ = stdin.Close()
		return nil, fmt.Errorf("open ssh stdout pipe: %w", err)
	}

	if err := cmd.Start(); err != nil {
		_ = stdin.Close()
		_ = stdout.Close()
		msg := strings.TrimSpace(stderr.String())
		if msg == "" {
			return nil, fmt.Errorf("start ssh dial command: %w", err)
		}
		return nil, fmt.Errorf("start ssh dial command: %w: %s", err, msg)
	}

	return &sshCommandConn{
		cmd:    cmd,
		stdin:  stdin,
		stdout: stdout,
	}, nil
}

func dialStdioRemoteArgs(target, socketPath string) []string {
	args := []string{remotePloyzdPath, "dial-stdio", "--socket", socketPath}
	if user := remoteUser(target); user != "" && user != "root" {
		return append([]string{"sudo", "-n"}, args...)
	}
	return args
}

type sshCommandConn struct {
	cmd    *exec.Cmd
	stdin  io.WriteCloser
	stdout io.ReadCloser

	mu     sync.Mutex
	closed bool
}

func (c *sshCommandConn) Read(p []byte) (int, error) {
	return c.stdout.Read(p)
}

func (c *sshCommandConn) Write(p []byte) (int, error) {
	return c.stdin.Write(p)
}

func (c *sshCommandConn) Close() error {
	c.mu.Lock()
	if c.closed {
		c.mu.Unlock()
		return nil
	}
	c.closed = true
	c.mu.Unlock()

	_ = c.stdin.Close()
	_ = c.stdout.Close()
	if c.cmd.Process != nil {
		_ = c.cmd.Process.Kill()
	}
	_ = c.cmd.Wait()
	return nil
}

func (c *sshCommandConn) LocalAddr() net.Addr {
	return stringAddr("ssh-local")
}

func (c *sshCommandConn) RemoteAddr() net.Addr {
	return stringAddr("ssh-remote")
}

func (c *sshCommandConn) SetDeadline(t time.Time) error {
	if rc, ok := c.stdout.(interface{ SetReadDeadline(time.Time) error }); ok {
		_ = rc.SetReadDeadline(t)
	}
	if wc, ok := c.stdin.(interface{ SetWriteDeadline(time.Time) error }); ok {
		_ = wc.SetWriteDeadline(t)
	}
	return nil
}

func (c *sshCommandConn) SetReadDeadline(t time.Time) error {
	if rc, ok := c.stdout.(interface{ SetReadDeadline(time.Time) error }); ok {
		return rc.SetReadDeadline(t)
	}
	return nil
}

func (c *sshCommandConn) SetWriteDeadline(t time.Time) error {
	if wc, ok := c.stdin.(interface{ SetWriteDeadline(time.Time) error }); ok {
		return wc.SetWriteDeadline(t)
	}
	return nil
}

type stringAddr string

func (a stringAddr) Network() string {
	return "ssh"
}

func (a stringAddr) String() string {
	return string(a)
}

func remoteUser(target string) string {
	target = strings.TrimSpace(target)
	if target == "" {
		return ""
	}
	parts := strings.SplitN(target, "@", 2)
	if len(parts) == 2 {
		return strings.TrimSpace(parts[0])
	}
	return ""
}
