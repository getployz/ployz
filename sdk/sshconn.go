package sdk

import (
	"io"
	"net"
	"os/exec"
	"time"
)

// sshConn wraps an SSH subprocess's stdin/stdout as a net.Conn.
type sshConn struct {
	cmd    *exec.Cmd
	stdin  io.WriteCloser
	stdout io.ReadCloser
}

func (c *sshConn) Read(b []byte) (int, error)  { return c.stdout.Read(b) }
func (c *sshConn) Write(b []byte) (int, error) { return c.stdin.Write(b) }

func (c *sshConn) Close() error {
	_ = c.stdin.Close()
	_ = c.stdout.Close()
	return c.cmd.Wait()
}

func (c *sshConn) LocalAddr() net.Addr                { return sshAddr{} }
func (c *sshConn) RemoteAddr() net.Addr               { return sshAddr{} }
func (c *sshConn) SetDeadline(_ time.Time) error      { return nil }
func (c *sshConn) SetReadDeadline(_ time.Time) error   { return nil }
func (c *sshConn) SetWriteDeadline(_ time.Time) error  { return nil }

type sshAddr struct{}

func (sshAddr) Network() string { return "ssh" }
func (sshAddr) String() string  { return "ssh" }
