package fake

import (
	"context"

	"ployz/internal/mesh"
)

var _ mesh.StatusProber = (*StatusProber)(nil)

// StatusProber returns canned status probe results.
type StatusProber struct {
	CallRecorder
	WG        bool
	DockerNet bool
	Corrosion bool

	ProbeInfraErr func(ctx context.Context, state *mesh.State) error
}

func (p *StatusProber) ProbeInfra(ctx context.Context, state *mesh.State) (bool, bool, bool, error) {
	p.record("ProbeInfra", state)
	if p.ProbeInfraErr != nil {
		if err := p.ProbeInfraErr(ctx, state); err != nil {
			return false, false, false, err
		}
	}
	return p.WG, p.DockerNet, p.Corrosion, nil
}
