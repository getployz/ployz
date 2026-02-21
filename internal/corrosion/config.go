package corrosion

import (
	"bytes"
	"fmt"
	"net/netip"
	"os"
	"os/user"
	"path/filepath"
	"strconv"
	"strings"
)

type Config struct {
	Dir          string
	SchemaPath   string
	ConfigPath   string
	AdminSock    string
	Bootstrap    []string
	GossipAddr   netip.AddrPort
	APIAddr      netip.AddrPort
	GossipMaxMTU int
	User         string
}

func WriteConfig(cfg Config) error {
	if err := os.MkdirAll(cfg.Dir, 0o700); err != nil {
		return fmt.Errorf("create corrosion dir: %w", err)
	}
	if err := os.WriteFile(cfg.SchemaPath, []byte(Schema), 0o644); err != nil {
		return fmt.Errorf("write corrosion schema: %w", err)
	}

	content := fmt.Sprintf(`[db]
path = %q
schema_paths = [%q]

[gossip]
addr = %q
bootstrap = %s
plaintext = true
max_mtu = %d
disable_gso = true

[api]
addr = %q

[admin]
path = %q
`, filepath.Join(cfg.Dir, "store.db"), cfg.SchemaPath, cfg.GossipAddr.String(), tomlStringArray(cfg.Bootstrap), cfg.GossipMaxMTU, cfg.APIAddr.String(), cfg.AdminSock)

	if err := os.WriteFile(cfg.ConfigPath, []byte(content), 0o600); err != nil {
		return fmt.Errorf("write corrosion config: %w", err)
	}
	if cfg.User != "" {
		if err := chownToUser(cfg.ConfigPath, cfg.User); err != nil {
			return err
		}
		if err := chownToUser(cfg.Dir, cfg.User); err != nil {
			return err
		}
	}
	return nil
}

func tomlStringArray(values []string) string {
	if len(values) == 0 {
		return "[]"
	}
	parts := make([]string, 0, len(values))
	for _, v := range values {
		if strings.TrimSpace(v) == "" {
			continue
		}
		parts = append(parts, fmt.Sprintf("%q", v))
	}
	if len(parts) == 0 {
		return "[]"
	}
	return "[" + strings.Join(parts, ", ") + "]"
}

func chownToUser(path, owner string) error {
	parts := bytes.Split([]byte(owner), []byte(":"))
	if len(parts) != 2 {
		return fmt.Errorf("invalid owner %q, expected uid:gid", owner)
	}
	uid, err := strconv.Atoi(string(parts[0]))
	if err != nil {
		if _, lookupErr := user.Lookup(string(parts[0])); lookupErr != nil {
			return fmt.Errorf("parse uid %q: %w", parts[0], err)
		}
		return nil
	}
	gid, err := strconv.Atoi(string(parts[1]))
	if err != nil {
		return fmt.Errorf("parse gid %q: %w", parts[1], err)
	}
	if err := os.Chown(path, uid, gid); err != nil {
		return fmt.Errorf("chown %s: %w", path, err)
	}
	return nil
}
