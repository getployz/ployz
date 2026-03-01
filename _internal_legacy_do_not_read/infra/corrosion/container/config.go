package container

import (
	"fmt"
	"net/netip"
	"os"
	"os/user"
	"path/filepath"
	"strconv"
	"strings"

	"ployz/pkg/sdk/defaults"
)

type Config struct {
	Dir          string
	ConfigPath   string
	AdminSock    string
	Bootstrap    []string
	GossipAddr   netip.AddrPort
	MemberID     uint64
	APIAddr      netip.AddrPort
	APIToken     string
	GossipMaxMTU int
	User         string
}

func WriteConfig(cfg Config) error {
	if cfg.MemberID == 0 {
		cfg.MemberID = 1
	}
	if err := defaults.EnsureDataRoot(cfg.Dir); err != nil {
		return fmt.Errorf("create corrosion dir: %w", err)
	}
	content := fmt.Sprintf(`[db]
path = %q

[gossip]
addr = %q
bootstrap = %s
plaintext = true
member_id = %d
max_mtu = %d
disable_gso = true

[api]
addr = %q

[admin]
path = %q
`, filepath.Join(cfg.Dir, "store.db"), cfg.GossipAddr.String(), tomlStringArray(cfg.Bootstrap), cfg.MemberID, cfg.GossipMaxMTU, cfg.APIAddr.String(), cfg.AdminSock)

	if err := os.WriteFile(cfg.ConfigPath, []byte(content), 0o644); err != nil {
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
	uidStr, gidStr, ok := strings.Cut(owner, ":")
	if !ok {
		return fmt.Errorf("invalid owner %q, expected uid:gid", owner)
	}
	uid, err := strconv.Atoi(uidStr)
	if err != nil {
		if _, lookupErr := user.Lookup(uidStr); lookupErr != nil {
			return fmt.Errorf("parse uid %q: %w", uidStr, err)
		}
		return nil
	}
	gid, err := strconv.Atoi(gidStr)
	if err != nil {
		return fmt.Errorf("parse gid %q: %w", gidStr, err)
	}
	if err := os.Chown(path, uid, gid); err != nil {
		return fmt.Errorf("chown %s: %w", path, err)
	}
	return nil
}
