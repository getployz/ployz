package cluster

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"
)

const envCluster = "PLOYZ_CLUSTER"

type Cluster struct {
	Network  string `json:"network,omitempty"`
	Socket   string `json:"socket,omitempty"`
	DataRoot string `json:"data_root,omitempty"`
}

type Config struct {
	CurrentCluster string             `json:"current_cluster,omitempty"`
	Clusters       map[string]Cluster `json:"clusters,omitempty"`

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
			return ".ployz-config.json"
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
			return cfg, nil
		}
		return nil, fmt.Errorf("read config file %q: %w", path, err)
	}
	if len(data) == 0 {
		return cfg, nil
	}
	if err := json.Unmarshal(data, cfg); err != nil {
		return nil, fmt.Errorf("parse config file %q: %w", path, err)
	}
	if cfg.Clusters == nil {
		cfg.Clusters = map[string]Cluster{}
	}
	cfg.path = path
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

	data, err := json.MarshalIndent(c, "", "  ")
	if err != nil {
		return fmt.Errorf("marshal config: %w", err)
	}
	tmp := c.path + ".tmp"
	if err := os.WriteFile(tmp, append(data, '\n'), 0o600); err != nil {
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
