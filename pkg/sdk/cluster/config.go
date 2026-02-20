package cluster

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"slices"
	"strings"

	"ployz/pkg/sdk/client"

	"gopkg.in/yaml.v3"
)

const envCluster = "PLOYZ_CLUSTER"

type Connection struct {
	Unix       string `yaml:"unix,omitempty" json:"-"`
	SSH        string `yaml:"ssh,omitempty" json:"-"`
	SSHKeyFile string `yaml:"ssh_key_file,omitempty" json:"-"`
	TCP        string `yaml:"tcp,omitempty" json:"-"`
	DataRoot   string `yaml:"data_root,omitempty" json:"-"`
}

func (c Connection) Validate() error {
	set := 0
	if c.Unix != "" {
		set++
	}
	if c.SSH != "" {
		set++
	}
	if c.TCP != "" {
		set++
	}
	if set == 0 {
		return fmt.Errorf("connection must set one of unix, ssh, or tcp")
	}
	if set > 1 {
		return fmt.Errorf("connection must set exactly one of unix, ssh, or tcp")
	}
	return nil
}

func (c Connection) Type() string {
	switch {
	case c.Unix != "":
		return "unix"
	case c.SSH != "":
		return "ssh"
	case c.TCP != "":
		return "tcp"
	default:
		return ""
	}
}

type Cluster struct {
	Network     string       `yaml:"network" json:"network,omitempty"`
	Connections []Connection `yaml:"connections" json:"-"`

	// Legacy JSON fields — used only for migration.
	Socket   string `yaml:"-" json:"socket,omitempty"`
	DataRoot string `yaml:"-" json:"data_root,omitempty"`
}

type Config struct {
	CurrentCluster string             `yaml:"current_cluster,omitempty" json:"current_cluster,omitempty"`
	Clusters       map[string]Cluster `yaml:"clusters,omitempty" json:"clusters,omitempty"`

	path string
}

func DefaultPath() string {
	if fromEnv := strings.TrimSpace(os.Getenv("PLOYZ_CONFIG")); fromEnv != "" {
		return fromEnv
	}
	dir, err := os.UserConfigDir()
	if err != nil {
		home, homeErr := os.UserHomeDir()
		if homeErr != nil {
			return filepath.Join(".config", "ployz", "config.yaml")
		}
		return filepath.Join(home, ".config", "ployz", "config.yaml")
	}
	return filepath.Join(dir, "ployz", "config.yaml")
}

func legacyJSONPath() string {
	dir, err := os.UserConfigDir()
	if err != nil {
		home, homeErr := os.UserHomeDir()
		if homeErr != nil {
			return filepath.Join(".config", "ployz", "config.json")
		}
		return filepath.Join(home, ".config", "ployz", "config.json")
	}
	return filepath.Join(dir, "ployz", "config.json")
}

func LoadDefault() (*Config, error) {
	return Load(DefaultPath())
}

func Load(path string) (*Config, error) {
	if strings.TrimSpace(path) == "" {
		path = DefaultPath()
	}

	cfg := &Config{path: path, Clusters: map[string]Cluster{}}

	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			// Try JSON migration.
			if migrated, migErr := migrateFromJSON(path); migErr == nil && migrated != nil {
				return migrated, nil
			}
			return cfg, nil
		}
		return nil, fmt.Errorf("read config file %q: %w", path, err)
	}
	if len(data) == 0 {
		return cfg, nil
	}
	if err := yaml.Unmarshal(data, cfg); err != nil {
		return nil, fmt.Errorf("parse config file %q: %w", path, err)
	}
	if cfg.Clusters == nil {
		cfg.Clusters = map[string]Cluster{}
	}
	cfg.path = path
	return cfg, nil
}

func migrateFromJSON(yamlPath string) (*Config, error) {
	jsonPath := legacyJSONPath()
	if fromEnv := strings.TrimSpace(os.Getenv("PLOYZ_CONFIG")); fromEnv != "" {
		// If PLOYZ_CONFIG was set, check for .json variant.
		jsonPath = strings.TrimSuffix(fromEnv, ".yaml") + ".json"
	}

	data, err := os.ReadFile(jsonPath)
	if err != nil {
		return nil, err
	}
	if len(data) == 0 {
		return nil, fmt.Errorf("empty json config")
	}

	// Parse old JSON format.
	var old struct {
		CurrentCluster string             `json:"current_cluster"`
		Clusters       map[string]Cluster `json:"clusters"`
	}
	if err := json.Unmarshal(data, &old); err != nil {
		return nil, fmt.Errorf("parse legacy json config: %w", err)
	}

	cfg := &Config{
		CurrentCluster: old.CurrentCluster,
		Clusters:       make(map[string]Cluster, len(old.Clusters)),
		path:           yamlPath,
	}

	for name, entry := range old.Clusters {
		conn := Connection{}
		if entry.Socket != "" {
			conn.Unix = entry.Socket
		} else {
			conn.Unix = client.DefaultSocketPath()
		}
		if entry.DataRoot != "" {
			conn.DataRoot = entry.DataRoot
		}
		cfg.Clusters[name] = Cluster{
			Network:     entry.Network,
			Connections: []Connection{conn},
		}
	}

	if err := cfg.Save(); err != nil {
		return nil, fmt.Errorf("save migrated yaml config: %w", err)
	}

	_ = os.Rename(jsonPath, jsonPath+".bak")
	return cfg, nil
}

func (c *Config) Save() error {
	if c == nil {
		return fmt.Errorf("config is nil")
	}
	if strings.TrimSpace(c.path) == "" {
		c.path = DefaultPath()
	}
	if c.Clusters == nil {
		c.Clusters = map[string]Cluster{}
	}

	dir := filepath.Dir(c.path)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return fmt.Errorf("create config directory %q: %w", dir, err)
	}

	data, err := yaml.Marshal(c)
	if err != nil {
		return fmt.Errorf("marshal config: %w", err)
	}
	tmp := c.path + ".tmp"
	if err := os.WriteFile(tmp, data, 0o600); err != nil {
		return fmt.Errorf("write temp config file %q: %w", tmp, err)
	}
	if err := os.Rename(tmp, c.path); err != nil {
		_ = os.Remove(tmp)
		return fmt.Errorf("replace config file %q: %w", c.path, err)
	}
	return nil
}

func (c *Config) Path() string {
	if c == nil {
		return ""
	}
	return c.path
}

func (c *Config) Current() (string, Cluster, bool) {
	if c == nil || len(c.Clusters) == 0 {
		return "", Cluster{}, false
	}
	if override := strings.TrimSpace(os.Getenv(envCluster)); override != "" {
		if cl, ok := c.Clusters[override]; ok {
			return override, cl, true
		}
	}
	if name := strings.TrimSpace(c.CurrentCluster); name != "" {
		if cl, ok := c.Clusters[name]; ok {
			return name, cl, true
		}
	}
	names := c.ClusterNames()
	if len(names) == 0 {
		return "", Cluster{}, false
	}
	name := names[0]
	return name, c.Clusters[name], true
}

func (c *Config) Cluster(name string) (Cluster, bool) {
	if c == nil {
		return Cluster{}, false
	}
	name = strings.TrimSpace(name)
	if name == "" {
		return Cluster{}, false
	}
	cl, ok := c.Clusters[name]
	return cl, ok
}

func (c *Config) Upsert(name string, cl Cluster) {
	if c.Clusters == nil {
		c.Clusters = map[string]Cluster{}
	}
	c.Clusters[strings.TrimSpace(name)] = cl
}

func (c *Config) Delete(name string) {
	if c == nil || c.Clusters == nil {
		return
	}
	delete(c.Clusters, strings.TrimSpace(name))
}

func (c *Config) ClusterNames() []string {
	if c == nil || len(c.Clusters) == 0 {
		return nil
	}
	names := make([]string, 0, len(c.Clusters))
	for name := range c.Clusters {
		names = append(names, name)
	}
	slices.Sort(names)
	return names
}

// Dial tries connections in order and returns the first successful client.
func (cl Cluster) Dial(ctx context.Context) (*client.Client, error) {
	if len(cl.Connections) == 0 {
		return nil, fmt.Errorf("cluster has no connections configured")
	}

	var lastErr error
	for _, conn := range cl.Connections {
		c, err := dialConnection(ctx, conn)
		if err != nil {
			lastErr = err
			continue
		}
		return c, nil
	}
	return nil, fmt.Errorf("all connections failed: %w", lastErr)
}

func dialConnection(_ context.Context, conn Connection) (*client.Client, error) {
	switch {
	case conn.Unix != "":
		return client.NewUnix(conn.Unix)

	case conn.SSH != "":
		target, port := parseSSHTarget(conn.SSH)
		socketPath := client.DefaultSocketPath()
		return client.NewSSH(target, client.SSHOptions{
			Port:       port,
			KeyPath:    conn.SSHKeyFile,
			SocketPath: socketPath,
		})

	case conn.TCP != "":
		return nil, fmt.Errorf("tcp connections not yet implemented")

	default:
		return nil, fmt.Errorf("invalid connection: no transport set")
	}
}

// parseSSHTarget splits "user@host:port" into target and port.
func parseSSHTarget(s string) (string, int) {
	// Check for user@host:port format.
	if idx := strings.LastIndex(s, ":"); idx > 0 {
		host := s[:idx]
		portStr := s[idx+1:]
		if _, err := net.LookupPort("tcp", portStr); err == nil {
			port := 0
			fmt.Sscanf(portStr, "%d", &port)
			if port > 0 {
				return host, port
			}
		}
	}
	return s, 0
}

// SocketPath returns the unix socket path from the first unix connection, or empty string.
func (cl Cluster) SocketPath() string {
	for _, conn := range cl.Connections {
		if conn.Unix != "" {
			return conn.Unix
		}
	}
	return ""
}

// DataRootFromConnections returns the data_root from the first connection that has one.
func (cl Cluster) DataRootFromConnections() string {
	for _, conn := range cl.Connections {
		if conn.DataRoot != "" {
			return conn.DataRoot
		}
	}
	return ""
}
