package container

import (
	"bytes"
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"net/netip"
	"os"
	"path/filepath"
	"strings"
	"time"

	"ployz/internal/adapter/wireguard"

	"github.com/containerd/errdefs"
	"github.com/docker/docker/api/types/container"
	"github.com/docker/docker/api/types/image"
	"github.com/docker/docker/api/types/mount"
	dockernetwork "github.com/docker/docker/api/types/network"
	"github.com/docker/docker/client"
	_ "modernc.org/sqlite"
)

type RuntimeConfig struct {
	Name       string
	Image      string
	ConfigPath string
	DataDir    string
	User       string
	APIAddr    netip.AddrPort
	APIToken   string
}

func Start(ctx context.Context, cli *client.Client, cfg RuntimeConfig) error {
	log := slog.With("component", "corrosion-runtime", "container", cfg.Name)
	log.Info("starting")
	_, err := cli.ContainerInspect(ctx, cfg.Name)
	if err == nil {
		log.Debug("removing existing container")
		if err := cli.ContainerRemove(ctx, cfg.Name, container.RemoveOptions{Force: true}); err != nil && !isRemoveOK(err) {
			return fmt.Errorf("remove old corrosion container: %w", err)
		}
		if err := waitContainerRemoved(ctx, cli, cfg.Name, 30*time.Second); err != nil {
			return fmt.Errorf("wait for old corrosion container removal: %w", err)
		}
	} else if !errdefs.IsNotFound(err) {
		return fmt.Errorf("inspect corrosion container: %w", err)
	}

	migrated, err := migrateLegacyStoreAddresses(ctx, cfg.DataDir)
	if err != nil {
		log.Warn("legacy address migration failed", "err", err)
	} else if migrated {
		log.Info("migrated legacy membership addresses")
	}

	if _, err := cli.ContainerCreate(ctx, containerConfig(cfg), hostConfig(cfg), nil, nil, cfg.Name); err != nil {
		if !errdefs.IsNotFound(err) {
			return fmt.Errorf("create corrosion container: %w", err)
		}
		log.Info("pulling image", "image", cfg.Image)
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
	log.Info("container started")
	if err := waitReady(ctx, cli, cfg.Name, cfg.APIAddr, cfg.APIToken, 30*time.Second); err != nil {
		return err
	}
	log.Info("api ready", "api_addr", cfg.APIAddr.String())
	if err := applySchema(ctx, cfg.APIAddr, cfg.APIToken); err != nil {
		return err
	}
	log.Info("schema applied")
	return nil
}

func Stop(ctx context.Context, cli *client.Client, name string) error {
	slog.Info("stopping corrosion runtime", "component", "corrosion-runtime", "container", name)
	if err := cli.ContainerStop(ctx, name, container.StopOptions{}); err != nil && !isRemoveOK(err) {
		return fmt.Errorf("stop corrosion container: %w", err)
	}
	if err := cli.ContainerRemove(ctx, name, container.RemoveOptions{Force: true}); err != nil && !isRemoveOK(err) {
		return fmt.Errorf("remove corrosion container: %w", err)
	}
	return nil
}

func isRemoveOK(err error) bool {
	if err == nil {
		return true
	}
	if errdefs.IsNotFound(err) {
		return true
	}
	return isRemovalInProgress(err)
}

func isRemovalInProgress(err error) bool {
	if err == nil {
		return false
	}
	msg := strings.ToLower(err.Error())
	return strings.Contains(msg, "already in progress") ||
		strings.Contains(msg, "already being removed") ||
		strings.Contains(msg, "marked for removal")
}

func waitContainerRemoved(ctx context.Context, cli *client.Client, name string, timeout time.Duration) error {
	deadline := time.NewTimer(timeout)
	defer deadline.Stop()
	ticker := time.NewTicker(250 * time.Millisecond)
	defer ticker.Stop()

	for {
		_, err := cli.ContainerInspect(ctx, name)
		switch {
		case err == nil:
		case errdefs.IsNotFound(err):
			return nil
		case isRemovalInProgress(err):
		default:
			return fmt.Errorf("inspect corrosion container: %w", err)
		}

		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-deadline.C:
			return fmt.Errorf("timeout after %s waiting for container %q removal", timeout, name)
		case <-ticker.C:
		}
	}
}

func waitReady(ctx context.Context, cli *client.Client, name string, apiAddr netip.AddrPort, apiToken string, timeout time.Duration) error {
	log := slog.With("component", "corrosion-runtime", "container", name, "api_addr", apiAddr.String())
	httpCli := &http.Client{Timeout: 2 * time.Second}
	body := []byte(`{"query":"SELECT 1","params":[]}`)
	url := "http://" + apiAddr.String() + "/v1/queries"

	deadline := time.NewTimer(timeout)
	defer deadline.Stop()
	ticker := time.NewTicker(500 * time.Millisecond)
	defer ticker.Stop()

	var lastErr string
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-deadline.C:
			msg := "corrosion not ready after " + timeout.String()
			if lastErr != "" {
				msg += ": " + lastErr
			}
			if logs := containerLogs(ctx, cli, name, 10); logs != "" {
				msg += "\n" + logs
			}
			log.Warn("readiness timeout", "detail", msg)
			return fmt.Errorf("%s", msg)
		case <-ticker.C:
			// fail fast if container exited
			info, inspectErr := cli.ContainerInspect(ctx, name)
			if inspectErr != nil {
				lastErr = "container not found"
				continue
			}
			if !info.State.Running {
				msg := fmt.Sprintf("corrosion container exited (status %d)", info.State.ExitCode)
				if logs := containerLogs(ctx, cli, name, 20); logs != "" {
					msg += "\n" + logs
				}
				log.Error("container exited before readiness", "exit_code", info.State.ExitCode)
				return fmt.Errorf("%s", msg)
			}

			req, err := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewReader(body))
			if err != nil {
				return fmt.Errorf("create readiness request: %w", err)
			}
			req.Header.Set("Content-Type", "application/json")
			req.Header.Set("Accept", "application/json")
			if apiToken != "" {
				req.Header.Set("Authorization", "Bearer "+apiToken)
			}

			resp, err := httpCli.Do(req)
			if err != nil {
				lastErr = err.Error()
				continue
			}

			if resp.StatusCode != http.StatusOK {
				data, _ := io.ReadAll(resp.Body)
				_ = resp.Body.Close()
				lastErr = fmt.Sprintf("status %d: %s", resp.StatusCode, bytes.TrimSpace(data))
				continue
			}

			var event struct {
				Error *string `json:"error"`
			}
			err = json.NewDecoder(resp.Body).Decode(&event)
			_ = resp.Body.Close()
			if err != nil {
				lastErr = "decode response: " + err.Error()
				continue
			}
			if event.Error != nil {
				lastErr = *event.Error
				continue
			}
			log.Debug("readiness probe succeeded")
			return nil
		}
	}
}

func applySchema(ctx context.Context, apiAddr netip.AddrPort, apiToken string) error {
	stmts := []string{
		"CREATE TABLE IF NOT EXISTS cluster (key TEXT NOT NULL PRIMARY KEY, value ANY)",
		"CREATE TABLE IF NOT EXISTS network_config (key TEXT NOT NULL PRIMARY KEY, value TEXT NOT NULL DEFAULT '')",
		"CREATE TABLE IF NOT EXISTS machines (id TEXT NOT NULL PRIMARY KEY, public_key TEXT NOT NULL DEFAULT '', subnet TEXT NOT NULL DEFAULT '', management_ip TEXT NOT NULL DEFAULT '', endpoint TEXT NOT NULL DEFAULT '', updated_at TEXT NOT NULL DEFAULT '', version INTEGER NOT NULL DEFAULT 1)",
		"CREATE TABLE IF NOT EXISTS heartbeats (node_id TEXT NOT NULL PRIMARY KEY, seq INTEGER NOT NULL DEFAULT 0, updated_at TEXT NOT NULL DEFAULT '')",
	}
	body, err := json.Marshal(stmts)
	if err != nil {
		return fmt.Errorf("marshal schema: %w", err)
	}
	url := "http://" + apiAddr.String() + "/v1/migrations"
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewReader(body))
	if err != nil {
		return fmt.Errorf("create schema request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Accept", "application/json")
	if apiToken != "" {
		req.Header.Set("Authorization", "Bearer "+apiToken)
	}
	resp, err := (&http.Client{Timeout: 10 * time.Second}).Do(req)
	if err != nil {
		return fmt.Errorf("apply schema: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		data, _ := io.ReadAll(resp.Body)
		return fmt.Errorf("apply schema: status %d: %s", resp.StatusCode, bytes.TrimSpace(data))
	}

	var out struct {
		Results []struct {
			Error *string `json:"error"`
		} `json:"results"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&out); err != nil {
		return fmt.Errorf("decode schema response: %w", err)
	}
	for _, result := range out.Results {
		if result.Error != nil && strings.TrimSpace(*result.Error) != "" {
			return fmt.Errorf("apply schema: %s", *result.Error)
		}
	}
	return nil
}

func containerLogs(ctx context.Context, cli *client.Client, name string, lines int) string {
	opts := container.LogsOptions{ShowStdout: true, ShowStderr: true, Tail: fmt.Sprintf("%d", lines)}
	rc, err := cli.ContainerLogs(ctx, name, opts)
	if err != nil {
		return ""
	}
	defer rc.Close()
	data, _ := io.ReadAll(rc)
	// strip docker stream framing (8-byte header per chunk)
	var clean []byte
	for len(data) >= 8 {
		size := int(data[4])<<24 | int(data[5])<<16 | int(data[6])<<8 | int(data[7])
		data = data[8:]
		if size > len(data) {
			size = len(data)
		}
		clean = append(clean, data[:size]...)
		data = data[size:]
	}
	return string(bytes.TrimSpace(clean))
}

func migrateLegacyStoreAddresses(ctx context.Context, dataDir string) (bool, error) {
	dir := strings.TrimSpace(dataDir)
	if dir == "" {
		return false, nil
	}
	dbPath := filepath.Join(dir, "store.db")
	if _, err := os.Stat(dbPath); err != nil {
		if os.IsNotExist(err) {
			return false, nil
		}
		return false, fmt.Errorf("stat corrosion store db: %w", err)
	}

	db, err := sql.Open("sqlite", dbPath)
	if err != nil {
		return false, fmt.Errorf("open corrosion store db: %w", err)
	}
	defer db.Close()

	if _, err := db.ExecContext(ctx, `PRAGMA busy_timeout = 5000`); err != nil {
		return false, fmt.Errorf("set corrosion sqlite busy timeout: %w", err)
	}

	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return false, fmt.Errorf("begin corrosion migration tx: %w", err)
	}

	machinesChanged, err := migrateMachinesManagementIPs(ctx, tx)
	if err != nil {
		_ = tx.Rollback()
		return false, err
	}
	membersChanged, err := migrateMembershipAddrs(ctx, tx)
	if err != nil {
		_ = tx.Rollback()
		return false, err
	}

	if !machinesChanged && !membersChanged {
		_ = tx.Rollback()
		return false, nil
	}
	if err := tx.Commit(); err != nil {
		return false, fmt.Errorf("commit corrosion migration tx: %w", err)
	}
	return true, nil
}

func migrateMachinesManagementIPs(ctx context.Context, tx *sql.Tx) (bool, error) {
	rows, err := tx.QueryContext(ctx, `SELECT id, public_key, management_ip FROM machines`)
	if err != nil {
		if missingTable(err) {
			return false, nil
		}
		return false, fmt.Errorf("query corrosion machines: %w", err)
	}
	defer rows.Close()

	changed := false
	for rows.Next() {
		var id string
		var publicKey string
		var managementIP string
		if err := rows.Scan(&id, &publicKey, &managementIP); err != nil {
			return false, fmt.Errorf("scan corrosion machine row: %w", err)
		}
		derived, err := wireguard.ManagementIPFromPublicKey(publicKey)
		if err != nil {
			continue
		}
		if strings.TrimSpace(managementIP) == derived.String() {
			continue
		}
		if _, err := tx.ExecContext(ctx, `UPDATE machines SET management_ip = ? WHERE id = ?`, derived.String(), id); err != nil {
			return false, fmt.Errorf("update corrosion machine management ip: %w", err)
		}
		changed = true
	}
	if err := rows.Err(); err != nil {
		return false, fmt.Errorf("iterate corrosion machines: %w", err)
	}
	return changed, nil
}

func migrateMembershipAddrs(ctx context.Context, tx *sql.Tx) (bool, error) {
	rows, err := tx.QueryContext(ctx, `SELECT actor_id, address, foca_state FROM __corro_members`)
	if err != nil {
		if missingTable(err) {
			return false, nil
		}
		return false, fmt.Errorf("query corrosion members: %w", err)
	}
	defer rows.Close()

	changed := false
	for rows.Next() {
		var actorID []byte
		var address string
		var focaState sql.NullString
		if err := rows.Scan(&actorID, &address, &focaState); err != nil {
			return false, fmt.Errorf("scan corrosion member row: %w", err)
		}

		migratedAddress, addressChanged := migrateLegacyAddrPort(address)
		focaChanged := false
		if focaState.Valid {
			migratedState, migrated, err := migrateLegacyFocaState(focaState.String)
			if err != nil {
				return false, err
			}
			if migrated {
				focaState.String = migratedState
				focaChanged = true
			}
		}

		if !addressChanged && !focaChanged {
			continue
		}

		if _, err := tx.ExecContext(ctx,
			`UPDATE __corro_members SET address = ?, foca_state = ? WHERE actor_id = ?`,
			migratedAddress,
			focaState,
			actorID,
		); err != nil {
			return false, fmt.Errorf("update corrosion member address: %w", err)
		}
		changed = true
	}
	if err := rows.Err(); err != nil {
		return false, fmt.Errorf("iterate corrosion members: %w", err)
	}
	return changed, nil
}

func migrateLegacyAddrPort(raw string) (string, bool) {
	addrPort := strings.TrimSpace(raw)
	if addrPort == "" {
		return raw, false
	}
	parsed, err := netip.ParseAddrPort(addrPort)
	if err != nil {
		return raw, false
	}
	migratedAddr, ok := wireguard.MigrateLegacyManagementAddr(parsed.Addr())
	if !ok {
		return raw, false
	}
	return netip.AddrPortFrom(migratedAddr, parsed.Port()).String(), true
}

func migrateLegacyFocaState(raw string) (string, bool, error) {
	state := strings.TrimSpace(raw)
	if state == "" {
		return raw, false, nil
	}

	var payload map[string]any
	if err := json.Unmarshal([]byte(state), &payload); err != nil {
		return raw, false, nil
	}
	idRaw, ok := payload["id"]
	if !ok {
		return raw, false, nil
	}
	idMap, ok := idRaw.(map[string]any)
	if !ok {
		return raw, false, nil
	}
	addrRaw, ok := idMap["addr"].(string)
	if !ok {
		return raw, false, nil
	}
	migratedAddr, changed := migrateLegacyAddrPort(addrRaw)
	if !changed {
		return raw, false, nil
	}
	idMap["addr"] = migratedAddr
	payload["id"] = idMap

	encoded, err := json.Marshal(payload)
	if err != nil {
		return raw, false, fmt.Errorf("marshal migrated corrosion member state: %w", err)
	}
	return string(encoded), true, nil
}

func missingTable(err error) bool {
	if err == nil {
		return false
	}
	return strings.Contains(strings.ToLower(err.Error()), "no such table")
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
