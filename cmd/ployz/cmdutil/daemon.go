package cmdutil

import (
	"context"
	"fmt"
	"path/filepath"
	"time"

	"ployz/daemon/pb"

	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

func IsDaemonRunning(_ context.Context, socketPath string) bool {
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	return HealthCheck(ctx, socketPath) == nil
}

func DaemonLogPath(dataRoot string) string {
	return filepath.Join(dataRoot, "ployzd.log")
}

func HealthCheck(ctx context.Context, socketPath string) error {
	checkCtx, cancel := context.WithTimeout(ctx, 2*time.Second)
	defer cancel()

	conn, err := grpc.NewClient(
		"unix://"+socketPath,
		grpc.WithTransportCredentials(insecure.NewCredentials()),
	)
	if err != nil {
		return fmt.Errorf("connect to daemon: %w", err)
	}
	defer func() { _ = conn.Close() }()

	client := pb.NewDaemonClient(conn)
	if _, err := client.GetStatus(checkCtx, &pb.GetStatusRequest{}); err != nil {
		return fmt.Errorf("daemon health check: %w", err)
	}
	return nil
}