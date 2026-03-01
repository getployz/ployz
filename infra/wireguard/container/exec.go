package container

import (
	"bytes"
	"context"
	"fmt"

	"github.com/docker/docker/api/types/container"
	"github.com/docker/docker/client"
	"github.com/docker/docker/pkg/stdcopy"
)

// exec runs a command inside the named container and returns its
// combined stdout. Stderr is captured separately for error reporting.
func exec(ctx context.Context, docker client.APIClient, name string, cmd ...string) ([]byte, error) {
	execCfg := container.ExecOptions{
		Cmd:          cmd,
		AttachStdout: true,
		AttachStderr: true,
	}

	resp, err := docker.ContainerExecCreate(ctx, name, execCfg)
	if err != nil {
		return nil, fmt.Errorf("create exec %v: %w", cmd, err)
	}

	attach, err := docker.ContainerExecAttach(ctx, resp.ID, container.ExecAttachOptions{})
	if err != nil {
		return nil, fmt.Errorf("attach exec %v: %w", cmd, err)
	}
	defer attach.Close()

	var stdout, stderr bytes.Buffer
	if _, err := stdcopy.StdCopy(&stdout, &stderr, attach.Reader); err != nil {
		return nil, fmt.Errorf("read exec output %v: %w", cmd, err)
	}

	info, err := docker.ContainerExecInspect(ctx, resp.ID)
	if err != nil {
		return nil, fmt.Errorf("inspect exec %v: %w", cmd, err)
	}
	if info.ExitCode != 0 {
		return nil, fmt.Errorf("exec %v: exit code %d: %s", cmd, info.ExitCode, stderr.String())
	}

	return stdout.Bytes(), nil
}
