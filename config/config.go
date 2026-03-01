// Package config handles CLI context configuration for connecting to daemons.
//
// Config is stored at $XDG_CONFIG_HOME/ployz/config.yaml (defaults to
// ~/.config/ployz/config.yaml) and follows the kubeconfig pattern: named
// contexts with a current-context selector.
package config

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"

	"gopkg.in/yaml.v3"
)

// Context describes how to connect to a ployz daemon.
type Context struct {
	Socket string `yaml:"socket,omitempty"` // unix socket path
	Host   string `yaml:"host,omitempty"`   // user@host for SSH
}

// Target returns the dial target for this context — Socket takes precedence.
func (c Context) Target() string {
	if c.Socket != "" {
		return c.Socket
	}
	return c.Host
}

// Config holds named daemon contexts and the current selection.
type Config struct {
	CurrentContext string             `yaml:"current-context"`
	Contexts       map[string]Context `yaml:"contexts"`
}

// Path returns the config file location. It respects XDG_CONFIG_HOME,
// falling back to ~/.config/ployz/config.yaml.
func Path() string {
	dir := os.Getenv("XDG_CONFIG_HOME")
	if dir == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return filepath.Join(".config", "ployz", "config.yaml")
		}
		dir = filepath.Join(home, ".config")
	}
	return filepath.Join(dir, "ployz", "config.yaml")
}

// Load reads the config file. If the file does not exist, an empty Config
// is returned (not an error).
func Load() (*Config, error) {
	data, err := os.ReadFile(Path())
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return &Config{Contexts: make(map[string]Context)}, nil
		}
		return nil, fmt.Errorf("read config: %w", err)
	}

	var cfg Config
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		return nil, fmt.Errorf("parse config: %w", err)
	}
	if cfg.Contexts == nil {
		cfg.Contexts = make(map[string]Context)
	}
	return &cfg, nil
}

// Save writes the config to disk, creating directories as needed.
func (c *Config) Save() error {
	p := Path()
	if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
		return fmt.Errorf("create config dir: %w", err)
	}

	data, err := yaml.Marshal(c)
	if err != nil {
		return fmt.Errorf("marshal config: %w", err)
	}
	if err := os.WriteFile(p, data, 0o644); err != nil {
		return fmt.Errorf("write config: %w", err)
	}
	return nil
}

// Current returns the current context name and value.
// The bool is false when no current context is set.
func (c *Config) Current() (string, Context, bool) {
	if c.CurrentContext == "" {
		return "", Context{}, false
	}
	ctx, ok := c.Contexts[c.CurrentContext]
	if !ok {
		return "", Context{}, false
	}
	return c.CurrentContext, ctx, true
}

// Use sets the current context. It returns an error if the name doesn't exist.
func (c *Config) Use(name string) error {
	if _, ok := c.Contexts[name]; !ok {
		return fmt.Errorf("context %q not found", name)
	}
	c.CurrentContext = name
	return nil
}

// Set adds or updates a named context.
func (c *Config) Set(name string, ctx Context) {
	c.Contexts[name] = ctx
}

// Remove deletes a context. If it was the current context, current-context
// is cleared. Returns an error if the name doesn't exist.
func (c *Config) Remove(name string) error {
	if _, ok := c.Contexts[name]; !ok {
		return fmt.Errorf("context %q not found", name)
	}
	delete(c.Contexts, name)
	if c.CurrentContext == name {
		c.CurrentContext = ""
	}
	return nil
}
