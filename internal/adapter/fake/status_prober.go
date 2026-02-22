package fake

import (
	"context"

	"ployz/internal/network"
)

var _ network.StatusProber = (*StatusProber)(nil)

// StatusProber returns canned status probe results.
type StatusProber struct {
	CallRecorder
	WG        bool
	DockerNet bool
	Corrosion bool

	ProbeInfraErr func(ctx context.Context, state *network.State) error
}

func (p *StatusProber) ProbeInfra(ctx context.Context, state *network.State) (bool, bool, bool, error) {
	p.record("ProbeInfra", state)
	if p.ProbeInfraErr != nil {
		if err := p.ProbeInfraErr(ctx, state); err != nil {
			return false, false, false, err
		}
	}
	return p.WG, p.DockerNet, p.Corrosion, nil
}
