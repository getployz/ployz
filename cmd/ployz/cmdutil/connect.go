package cmdutil

import (
	"context"
	"fmt"
	"os"

	"ployz/config"
	"ployz/platform"
	"ployz/sdk"
)

// Connect returns an SDK client by resolving the target from flags, env vars,
// auto-discovery, or the config file's current-context. Resolution order:
//
//  1. hostFlag / PLOYZ_HOST
//  2. contextFlag / PLOYZ_CONTEXT
//  3. Auto-discovered local daemon
//  4. current-context from config file
func Connect(ctx context.Context, hostFlag, contextFlag string) (*sdk.Client, error) {
	// 1. Direct host (flag > env).
	host := firstNonEmpty(hostFlag, os.Getenv("PLOYZ_HOST"))
	if host != "" {
		return sdk.Dial(ctx, host)
	}

	// 2. Named context (flag > env).
	ctxName := firstNonEmpty(contextFlag, os.Getenv("PLOYZ_CONTEXT"))
	if ctxName != "" {
		return dialContext(ctx, ctxName)
	}

	// 3. Auto-discover local daemon.
	if IsDaemonRunning(ctx, platform.DaemonSocketPath) {
		return sdk.Dial(ctx, platform.DaemonSocketPath)
	}

	// 4. Fall back to config's current-context.
	cfg, err := config.Load()
	if err != nil {
		return nil, fmt.Errorf("load config: %w", err)
	}
	name, c, ok := cfg.Current()
	if !ok {
		return nil, fmt.Errorf("no context configured — run a daemon or add a context")
	}
	target := c.Target()
	if target == "" {
		return nil, fmt.Errorf("context %q has no target", name)
	}
	return sdk.Dial(ctx, target)
}

// Discover checks whether the local daemon is alive and, if so, upserts
// the "local" context in config. It does not change current-context if one
// is already set.
func Discover(ctx context.Context) error {
	if !IsDaemonRunning(ctx, platform.DaemonSocketPath) {
		return nil
	}

	cfg, err := config.Load()
	if err != nil {
		return err
	}

	cfg.Set("local", config.Context{Socket: platform.DaemonSocketPath})

	if cfg.CurrentContext == "" {
		cfg.CurrentContext = "local"
	}

	return cfg.Save()
}

func dialContext(ctx context.Context, name string) (*sdk.Client, error) {
	cfg, err := config.Load()
	if err != nil {
		return nil, fmt.Errorf("load config: %w", err)
	}
	c, ok := cfg.Contexts[name]
	if !ok {
		return nil, fmt.Errorf("context %q not found", name)
	}
	target := c.Target()
	if target == "" {
		return nil, fmt.Errorf("context %q has no target", name)
	}
	return sdk.Dial(ctx, target)
}

func firstNonEmpty(values ...string) string {
	for _, v := range values {
		if v != "" {
			return v
		}
	}
	return ""
}
