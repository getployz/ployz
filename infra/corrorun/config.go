// Package corrorun manages the Corrosion process lifecycle.
// Three modes: container (Docker), exec (child process), and service (external).
// All implement the Start/Stop portion of mesh.Store.
package corrorun

import (
	"bytes"
	"fmt"
	"net/netip"
	"os"
	"path/filepath"

	"github.com/BurntSushi/toml"
)

const (
	DefaultGossipPort = 51001
	DefaultAPIPort    = 51002
)

// Config is the Corrosion TOML configuration.
type Config struct {
	DB     DBConfig     `toml:"db"`
	Gossip GossipConfig `toml:"gossip"`
	API    APIConfig    `toml:"api"`
	Admin  AdminConfig  `toml:"admin"`
}

type DBConfig struct {
	Path        string   `toml:"path"`
	SchemaPaths []string `toml:"schema_paths"`
}

type GossipConfig struct {
	Addr      string   `toml:"addr"`
	Bootstrap []string `toml:"bootstrap"`
	Plaintext bool     `toml:"plaintext"`
}

type APIConfig struct {
	Addr string `toml:"addr"`
}

type AdminConfig struct {
	Path string `toml:"path"`
}

// Paths holds the filesystem paths for a Corrosion data directory.
type Paths struct {
	Dir    string // root corrosion data dir
	Config string // config.toml
	Schema string // schema.sql
	DB     string // store.db
	Admin  string // admin.sock
}

// NewPaths derives all paths from a root data directory.
func NewPaths(dataDir string) Paths {
	dir := filepath.Join(dataDir, "corrosion")
	return Paths{
		Dir:    dir,
		Config: filepath.Join(dir, "config.toml"),
		Schema: filepath.Join(dir, "schema.sql"),
		DB:     filepath.Join(dir, "store.db"),
		Admin:  filepath.Join(dir, "admin.sock"),
	}
}

// WriteConfig writes the Corrosion config.toml and schema.sql to disk.
// The schema parameter is the SQL schema to apply on startup (typically store.Schema).
func WriteConfig(paths Paths, schema string, gossipAddr netip.AddrPort, apiAddr netip.AddrPort, bootstrap []string) error {
	if err := os.MkdirAll(paths.Dir, 0o700); err != nil {
		return fmt.Errorf("create corrosion dir: %w", err)
	}

	cfg := Config{
		DB: DBConfig{
			Path:        paths.DB,
			SchemaPaths: []string{paths.Schema},
		},
		Gossip: GossipConfig{
			Addr:      gossipAddr.String(),
			Bootstrap: bootstrap,
			Plaintext: true,
		},
		API: APIConfig{
			Addr: apiAddr.String(),
		},
		Admin: AdminConfig{
			Path: paths.Admin,
		},
	}

	var buf bytes.Buffer
	if err := toml.NewEncoder(&buf).Encode(cfg); err != nil {
		return fmt.Errorf("encode corrosion config: %w", err)
	}
	if err := os.WriteFile(paths.Config, buf.Bytes(), 0o600); err != nil {
		return fmt.Errorf("write corrosion config: %w", err)
	}

	if err := os.WriteFile(paths.Schema, []byte(schema), 0o644); err != nil {
		return fmt.Errorf("write corrosion schema: %w", err)
	}

	return nil
}
