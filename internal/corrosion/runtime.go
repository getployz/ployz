package corrosion

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/netip"
	"time"

	"github.com/containerd/errdefs"
	"github.com/docker/docker/api/types/container"
	"github.com/docker/docker/api/types/image"
	"github.com/docker/docker/api/types/mount"
	dockernetwork "github.com/docker/docker/api/types/network"
	"github.com/docker/docker/client"
)

type RuntimeConfig struct {
	Name       string
	Image      string
	ConfigPath string
	DataDir    string
	User       string
	APIAddr    netip.AddrPort
}

func Start(ctx context.Context, cli *client.Client, cfg RuntimeConfig) error {
	_, err := cli.ContainerInspect(ctx, cfg.Name)
	if err == nil {
		if err := cli.ContainerRemove(ctx, cfg.Name, container.RemoveOptions{Force: true}); err != nil {
			return fmt.Errorf("remove old corrosion container: %w", err)
		}
	} else if !errdefs.IsNotFound(err) {
		return fmt.Errorf("inspect corrosion container: %w", err)
	}

	if _, err := cli.ContainerCreate(ctx, containerConfig(cfg), hostConfig(cfg), nil, nil, cfg.Name); err != nil {
		if !errdefs.IsNotFound(err) {
			return fmt.Errorf("create corrosion container: %w", err)
		}
		pull, pullErr := cli.ImagePull(ctx, cfg.Image, image.PullOptions{})
		if pullErr != nil {
			return fmt.Errorf("pull corrosion image: %w", pullErr)
		}
		_, _ = io.Copy(io.Discard, pull)
		_ = pull.Close()
		if _, err = cli.ContainerCreate(ctx, containerConfig(cfg), hostConfig(cfg), nil, nil, cfg.Name); err != nil {
			return fmt.Errorf("create corrosion container after pull: %w", err)
		}
	}

	if err := cli.ContainerStart(ctx, cfg.Name, container.StartOptions{}); err != nil {
		return fmt.Errorf("start corrosion container: %w", err)
	}
	if err := WaitReady(ctx, cfg.APIAddr, 20*time.Second); err != nil {
		return err
	}
	return nil
}

func Stop(ctx context.Context, cli *client.Client, name string) error {
	if err := cli.ContainerStop(ctx, name, container.StopOptions{}); err != nil && !errdefs.IsNotFound(err) {
		return fmt.Errorf("stop corrosion container: %w", err)
	}
	if err := cli.ContainerRemove(ctx, name, container.RemoveOptions{}); err != nil && !errdefs.IsNotFound(err) {
		return fmt.Errorf("remove corrosion container: %w", err)
	}
	return nil
}

func WaitReady(ctx context.Context, apiAddr netip.AddrPort, timeout time.Duration) error {
	client := &http.Client{Timeout: 2 * time.Second}
	body := []byte(`{"query":"SELECT 1 FROM cluster LIMIT 1","params":[]}`)
	url := "http://" + apiAddr.String() + "/v1/queries"

	deadline := time.NewTimer(timeout)
	defer deadline.Stop()
	ticker := time.NewTicker(300 * time.Millisecond)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-deadline.C:
			return fmt.Errorf("corrosion not ready after timeout")
		case <-ticker.C:
			req, err := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewReader(body))
			if err != nil {
				return fmt.Errorf("create readiness request: %w", err)
			}
			req.Header.Set("Content-Type", "application/json")
			req.Header.Set("Accept", "application/json")

			resp, err := client.Do(req)
			if err != nil {
				continue
			}

			if resp.StatusCode != http.StatusOK {
				_ = resp.Body.Close()
				continue
			}

			var event struct {
				Error *string `json:"error"`
			}
			err = json.NewDecoder(resp.Body).Decode(&event)
			_ = resp.Body.Close()
			if err != nil {
				continue
			}
			if event.Error != nil {
				continue
			}
			return nil
		}
	}
}

func containerConfig(cfg RuntimeConfig) *container.Config {
	return &container.Config{
		Image: cfg.Image,
		Cmd:   []string{"corrosion", "agent", "-c", cfg.ConfigPath},
		User:  cfg.User,
	}
}

func hostConfig(cfg RuntimeConfig) *container.HostConfig {
	return &container.HostConfig{
		NetworkMode: dockernetwork.NetworkHost,
		RestartPolicy: container.RestartPolicy{
			Name: container.RestartPolicyAlways,
		},
		Mounts: []mount.Mount{
			{
				Type:   mount.TypeBind,
				Source: cfg.DataDir,
				Target: cfg.DataDir,
			},
		},
	}
}
