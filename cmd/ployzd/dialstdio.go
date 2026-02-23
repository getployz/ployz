package main

import (
	"context"
	"fmt"
	"io"
	"net"
	"os"

	"ployz/pkg/sdk/client"

	"github.com/spf13/cobra"
)

type halfReadCloser interface {
	io.Reader
	CloseRead() error
}

type halfWriteCloser interface {
	io.Writer
	CloseWrite() error
}

type halfReadCloserWrapper struct {
	io.ReadCloser
}

func (x *halfReadCloserWrapper) CloseRead() error {
	return x.Close()
}

type halfWriteCloserWrapper struct {
	io.WriteCloser
}

func (x *halfWriteCloserWrapper) CloseWrite() error {
	return x.Close()
}

func dialStdioCmd() *cobra.Command {
	var socketPath string

	cmd := &cobra.Command{
		Use:    "dial-stdio",
		Short:  "Proxy stdio to ployzd socket",
		Hidden: true,
		RunE: func(cmd *cobra.Command, args []string) error {
			return runDialStdio(cmd.Context(), socketPath, os.Stdin, os.Stdout)
		},
	}

	cmd.Flags().StringVar(&socketPath, "socket", client.DefaultSocketPath(), "Path to the ployzd Unix socket")
	return cmd
}

func runDialStdio(ctx context.Context, socketPath string, stdin io.Reader, stdout io.Writer) error {
	var dialer net.Dialer
	conn, err := dialer.DialContext(ctx, "unix", socketPath)
	if err != nil {
		return fmt.Errorf("connect to socket %q: %w", socketPath, err)
	}
	defer conn.Close()

	var stdinCloser halfReadCloser
	if c, ok := stdin.(halfReadCloser); ok {
		stdinCloser = c
	} else if c, ok := stdin.(io.ReadCloser); ok {
		stdinCloser = &halfReadCloserWrapper{c}
	}

	var stdoutCloser halfWriteCloser
	if c, ok := stdout.(halfWriteCloser); ok {
		stdoutCloser = c
	} else if c, ok := stdout.(io.WriteCloser); ok {
		stdoutCloser = &halfWriteCloserWrapper{c}
	}

	stdinToSocket := make(chan error, 1)
	socketToStdout := make(chan error, 1)

	go func() {
		_, copyErr := io.Copy(conn, stdin)
		stdinToSocket <- copyErr
		if unixConn, ok := conn.(*net.UnixConn); ok {
			_ = unixConn.CloseWrite()
		}
		if stdinCloser != nil {
			_ = stdinCloser.CloseRead()
		}
	}()

	go func() {
		_, copyErr := io.Copy(stdout, conn)
		socketToStdout <- copyErr
		if unixConn, ok := conn.(*net.UnixConn); ok {
			_ = unixConn.CloseRead()
		}
		if stdoutCloser != nil {
			_ = stdoutCloser.CloseWrite()
		}
	}()

	select {
	case err = <-stdinToSocket:
		if err != nil {
			return err
		}
		err = <-socketToStdout
	case err = <-socketToStdout:
	}
	return err
}
