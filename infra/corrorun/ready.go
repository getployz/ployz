package corrorun

import (
	"context"
	"fmt"
	"net/netip"
	"time"

	"github.com/cenkalti/backoff/v4"

	"ployz/infra/corrosion"
)

const (
	readyInitialInterval = 50 * time.Millisecond
	readyMaxInterval     = 1 * time.Second
	readyMaxElapsed      = 15 * time.Second
)

// WaitReady blocks until Corrosion is accepting queries and the schema is applied.
//
// This is intentionally a minimal check — it only verifies the API is up and
// the machines table exists. It does NOT gate on replication health (gaps,
// queue_size, member count).
//
// Why: a node that was offline while peers were removed will start with
// unresolvable gaps from actors it can never sync with. Gating on gaps=0
// here would deadlock startup. Cluster health is the convergence layer's
// responsibility — it has the full picture (store records + WireGuard peer
// reachability) and can distinguish "gap from a dead peer" from "gap from
// a live peer I haven't caught up with yet."
//
// See corrosion.Client.HealthContext for the full health endpoint, which is
// available for diagnostics and status reporting after startup.
func WaitReady(ctx context.Context, apiAddr netip.AddrPort) error {
	client, err := corrosion.NewClient(apiAddr)
	if err != nil {
		return fmt.Errorf("wait ready: create client: %w", err)
	}

	check := func() error {
		rows, err := client.QueryContext(ctx, "SELECT 1 FROM machines LIMIT 1")
		if err != nil {
			return err
		}
		rows.Close()
		return nil
	}

	b := backoff.NewExponentialBackOff(
		backoff.WithInitialInterval(readyInitialInterval),
		backoff.WithMaxInterval(readyMaxInterval),
		backoff.WithMaxElapsedTime(readyMaxElapsed),
	)
	if err := backoff.Retry(check, backoff.WithContext(b, ctx)); err != nil {
		return fmt.Errorf("wait ready: corrosion not responding after %s: %w", readyMaxElapsed, err)
	}
	return nil
}
