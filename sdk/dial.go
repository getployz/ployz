package sdk

import (
	"context"
	"fmt"
	"net"
	"os/exec"
	"strconv"

	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

func dialUnix(_ context.Context, socketPath string) (*grpc.ClientConn, error) {
	conn, err := grpc.NewClient(
		"unix://"+socketPath,
		grpc.WithTransportCredentials(insecure.NewCredentials()),
	)
	if err != nil {
		return nil, fmt.Errorf("dial unix %s: %w", socketPath, err)
	}
	return conn, nil
}

func dialSSH(ctx context.Context, target string, cfg dialConfig) (*grpc.ClientConn, error) {
	conn, err := grpc.NewClient(
		"passthrough:///ssh",
		grpc.WithTransportCredentials(insecure.NewCredentials()),
		grpc.WithContextDialer(func(ctx context.Context, _ string) (net.Conn, error) {
			return startSSH(ctx, target, cfg)
		}),
	)
	if err != nil {
		return nil, fmt.Errorf("dial ssh %s: %w", target, err)
	}
	return conn, nil
}

func startSSH(ctx context.Context, target string, cfg dialConfig) (net.Conn, error) {
	args := []string{target}
	if cfg.sshPort != 0 {
		args = append(args, "-p", strconv.Itoa(cfg.sshPort))
	}

	remoteCmd := "ployzd dial-stdio"
	if cfg.remoteSocketPath != "" {
		remoteCmd += " --socket " + cfg.remoteSocketPath
	}
	args = append(args, remoteCmd)

	cmd := exec.CommandContext(ctx, "ssh", args...)

	stdin, err := cmd.StdinPipe()
	if err != nil {
		return nil, fmt.Errorf("ssh stdin pipe: %w", err)
	}

	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return nil, fmt.Errorf("ssh stdout pipe: %w", err)
	}

	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("start ssh: %w", err)
	}

	return &sshConn{cmd: cmd, stdin: stdin, stdout: stdout}, nil
}
