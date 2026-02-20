package machine

import (
	"context"
	"errors"
	"fmt"
	"os"
	"strings"
)

func (c *Controller) AddPeer(ctx context.Context, in Config, peer Peer) (Config, error) {
	cfg, err := NormalizeConfig(in)
	if err != nil {
		return Config{}, err
	}

	s, err := loadState(cfg.DataDir)
	if err != nil {
		return Config{}, fmt.Errorf("load state: %w", err)
	}

	norm, err := normalizePeer(peer)
	if err != nil {
		return Config{}, err
	}

	replaced := false
	for i := range s.Peers {
		if strings.EqualFold(s.Peers[i].PublicKey, norm.PublicKey) {
			s.Peers[i] = norm
			replaced = true
			break
		}
	}
	if !replaced {
		s.Peers = append(s.Peers, norm)
	}

	if err := saveState(cfg.DataDir, s); err != nil {
		return Config{}, err
	}

	cfg, err = hydrateConfigFromState(cfg, s)
	if err != nil {
		return Config{}, err
	}
	if s.Running {
		if err := c.applyPeerConfig(ctx, cfg, s); err != nil {
			return Config{}, err
		}
	}
	return cfg, nil
}

func (c *Controller) RemovePeer(ctx context.Context, in Config, publicKey string) (Config, error) {
	cfg, err := NormalizeConfig(in)
	if err != nil {
		return Config{}, err
	}

	s, err := loadState(cfg.DataDir)
	if err != nil {
		return Config{}, fmt.Errorf("load state: %w", err)
	}

	pk := strings.TrimSpace(publicKey)
	if pk == "" {
		return Config{}, fmt.Errorf("public key is required")
	}

	idx := -1
	for i := range s.Peers {
		if strings.EqualFold(s.Peers[i].PublicKey, pk) {
			idx = i
			break
		}
	}
	if idx == -1 {
		return Config{}, fmt.Errorf("peer %q not found", pk)
	}
	s.Peers = append(s.Peers[:idx], s.Peers[idx+1:]...)

	if err := saveState(cfg.DataDir, s); err != nil {
		return Config{}, err
	}

	cfg, err = hydrateConfigFromState(cfg, s)
	if err != nil {
		return Config{}, err
	}
	if s.Running {
		if err := c.applyPeerConfig(ctx, cfg, s); err != nil {
			return Config{}, err
		}
	}
	return cfg, nil
}

func (c *Controller) ListPeers(_ context.Context, in Config) ([]Peer, string, error) {
	cfg, err := NormalizeConfig(in)
	if err != nil {
		return nil, "", err
	}
	s, err := loadState(cfg.DataDir)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil, statePath(cfg.DataDir), nil
		}
		return nil, "", fmt.Errorf("load state: %w", err)
	}
	return s.Peers, statePath(cfg.DataDir), nil
}
