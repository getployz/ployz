package overlay

import (
	"context"
	"fmt"
	"net/netip"
	"strings"
	"time"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

func (c *Controller) Reconcile(ctx context.Context, in Config) (int, error) {
	cfg, err := NormalizeConfig(in)
	if err != nil {
		return 0, err
	}

	s, err := c.state.Load(cfg.DataDir)
	if err != nil {
		return 0, fmt.Errorf("load state: %w", err)
	}

	cfg, err = Resolve(cfg, s)
	if err != nil {
		return 0, err
	}

	r := c.newRegistry(cfg.CorrosionAPIAddr, cfg.CorrosionAPIToken)
	if err := r.EnsureMachineTable(ctx); err != nil {
		return 0, err
	}
	if err := r.EnsureNetworkConfigTable(ctx); err != nil {
		return 0, err
	}

	cidr, err := r.EnsureNetworkCIDR(ctx, cfg.NetworkCIDR, s.CIDR, defaultNetworkPrefix)
	if err != nil {
		return 0, err
	}
	s.CIDR = cidr.String()

	if err := r.UpsertMachine(ctx, MachineRow{
		ID:           s.WGPublic,
		PublicKey:    s.WGPublic,
		Subnet:       s.Subnet,
		ManagementIP: s.Management,
		Endpoint:     s.Advertise,
		UpdatedAt:    c.clock.Now().UTC().Format(time.RFC3339),
	}); err != nil {
		return 0, err
	}

	rows, err := r.ListMachineRows(ctx)
	if err != nil {
		return 0, err
	}
	return c.reconcilePeerRows(ctx, cfg, s, rows)
}

func (c *Controller) ReconcilePeers(ctx context.Context, in Config, rows []MachineRow) (int, error) {
	s, err := c.state.Load(in.DataDir)
	if err != nil {
		return 0, fmt.Errorf("load state: %w", err)
	}

	return c.reconcilePeerRows(ctx, in, s, rows)
}

func (c *Controller) ListMachines(ctx context.Context, in Config) ([]Machine, error) {
	cfg, err := NormalizeConfig(in)
	if err != nil {
		return nil, err
	}

	r := c.newRegistry(cfg.CorrosionAPIAddr, cfg.CorrosionAPIToken)
	if err := r.EnsureMachineTable(ctx); err != nil {
		return nil, err
	}
	rows, err := r.ListMachineRows(ctx)
	if err != nil {
		return nil, err
	}
	normalized, err := normalizeRegistryRows(rows)
	if err != nil {
		return nil, err
	}

	out := make([]Machine, 0, len(normalized))
	for _, row := range normalized {
		out = append(out, Machine{
			ID:           row.ID,
			PublicKey:    row.PublicKey,
			Subnet:       row.Subnet,
			ManagementIP: row.ManagementIP,
			Endpoint:     row.Endpoint,
			LastUpdated:  row.UpdatedAt,
			Version:      row.Version,
		})
	}
	return out, nil
}

func (c *Controller) UpsertMachine(ctx context.Context, in Config, m Machine) error {
	cfg, err := NormalizeConfig(in)
	if err != nil {
		return err
	}

	s, err := c.state.Load(cfg.DataDir)
	if err != nil {
		return fmt.Errorf("load state: %w", err)
	}
	cfg, err = Resolve(cfg, s)
	if err != nil {
		return err
	}

	m.ID = strings.TrimSpace(m.ID)
	m.PublicKey = strings.TrimSpace(m.PublicKey)
	m.Subnet = strings.TrimSpace(m.Subnet)
	m.Endpoint = strings.TrimSpace(m.Endpoint)

	if m.ID == "" {
		m.ID = m.PublicKey
	}
	if m.ID == "" {
		return fmt.Errorf("machine id is required")
	}
	if m.PublicKey == "" {
		return fmt.Errorf("machine public key is required")
	}
	if _, err := wgtypes.ParseKey(m.PublicKey); err != nil {
		return fmt.Errorf("parse machine public key: %w", err)
	}
	managementIP, err := ManagementIPFromPublicKey(m.PublicKey)
	if err != nil {
		return fmt.Errorf("derive machine management IP: %w", err)
	}
	m.ManagementIP = managementIP.String()
	if m.Subnet == "" {
		return fmt.Errorf("machine subnet is required")
	}
	if _, err := netip.ParsePrefix(m.Subnet); err != nil {
		return fmt.Errorf("parse machine subnet: %w", err)
	}
	if m.Endpoint != "" {
		if _, err := netip.ParseAddrPort(m.Endpoint); err != nil {
			return fmt.Errorf("parse machine endpoint: %w", err)
		}
	}

	r := c.newRegistry(cfg.CorrosionAPIAddr, cfg.CorrosionAPIToken)
	if err := r.EnsureMachineTable(ctx); err != nil {
		return err
	}
	if err := r.EnsureNetworkConfigTable(ctx); err != nil {
		return err
	}
	if _, err := r.EnsureNetworkCIDR(ctx, cfg.NetworkCIDR, s.CIDR, defaultNetworkPrefix); err != nil {
		return err
	}

	if m.Endpoint != "" {
		if err := r.DeleteByEndpointExceptID(ctx, m.Endpoint, m.ID); err != nil {
			return err
		}
	}

	return r.UpsertMachine(ctx, MachineRow{
		ID:           m.ID,
		PublicKey:    m.PublicKey,
		Subnet:       m.Subnet,
		ManagementIP: m.ManagementIP,
		Endpoint:     m.Endpoint,
		UpdatedAt:    c.clock.Now().UTC().Format(time.RFC3339),
	})
}

func (c *Controller) RemoveMachine(ctx context.Context, in Config, machineID string) error {
	cfg, err := NormalizeConfig(in)
	if err != nil {
		return err
	}
	machineID = strings.TrimSpace(machineID)
	if machineID == "" {
		return fmt.Errorf("machine id is required")
	}

	r := c.newRegistry(cfg.CorrosionAPIAddr, cfg.CorrosionAPIToken)
	if err := r.EnsureMachineTable(ctx); err != nil {
		return err
	}
	if err := r.DeleteMachine(ctx, machineID); err != nil {
		return err
	}
	_, err = c.Reconcile(ctx, cfg)
	return err
}

func normalizeRegistryRows(rows []MachineRow) ([]MachineRow, error) {
	out := make([]MachineRow, len(rows))
	for i, row := range rows {
		managementIP, err := ManagementIPFromPublicKey(row.PublicKey)
		if err != nil {
			return nil, fmt.Errorf("derive machine management ip: %w", err)
		}
		row.ManagementIP = managementIP.String()
		out[i] = row
	}
	return out, nil
}

func (c *Controller) reconcilePeerRows(ctx context.Context, cfg Config, s *State, rows []MachineRow) (int, error) {
	normalized, err := normalizeRegistryRows(rows)
	if err != nil {
		return 0, err
	}

	peers := make([]Peer, 0, len(normalized))
	for _, row := range normalized {
		if row.PublicKey == s.WGPublic {
			continue
		}
		peers = append(peers, Peer{
			PublicKey:    row.PublicKey,
			Subnet:       row.Subnet,
			ManagementIP: row.ManagementIP,
			Endpoint:     row.Endpoint,
		})
	}

	if err := c.state.Save(cfg.DataDir, s); err != nil {
		return 0, err
	}

	if err := c.platformOps.ApplyPeerConfig(ctx, cfg, s, peers); err != nil {
		return 0, err
	}

	return len(peers), nil
}
