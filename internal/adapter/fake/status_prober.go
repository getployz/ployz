package fake

import (
	"context"
	"sync"

	"ployz/internal/adapter/fake/fault"
	"ployz/internal/check"
	"ployz/internal/mesh"
)

var _ mesh.StatusProber = (*StatusProber)(nil)

const FaultStatusProberProbeInfra = "status_prober.probe_infra"

// StatusProber returns canned status probe results.
type StatusProber struct {
	CallRecorder
	WG        bool
	DockerNet bool
	Corrosion bool
	faultMu   sync.Mutex
	faults    *fault.Injector

	ProbeInfraErr func(ctx context.Context, state *mesh.State) error
}

func (p *StatusProber) FailOnce(point string, err error) {
	p.ensureFaults().FailOnce(point, err)
}

func (p *StatusProber) FailAlways(point string, err error) {
	p.ensureFaults().FailAlways(point, err)
}

func (p *StatusProber) SetFaultHook(point string, hook fault.Hook) {
	p.ensureFaults().SetHook(point, hook)
}

func (p *StatusProber) ClearFault(point string) {
	p.ensureFaults().Clear(point)
}

func (p *StatusProber) ResetFaults() {
	p.ensureFaults().Reset()
}

func (p *StatusProber) evalFault(point string, args ...any) error {
	check.Assert(p != nil, "StatusProber.evalFault: receiver must not be nil")
	if p == nil {
		return nil
	}
	return p.ensureFaults().Eval(point, args...)
}

func (p *StatusProber) ensureFaults() *fault.Injector {
	p.faultMu.Lock()
	defer p.faultMu.Unlock()
	if p.faults == nil {
		p.faults = fault.NewInjector()
	}
	return p.faults
}

func (p *StatusProber) ProbeInfra(ctx context.Context, state *mesh.State) (bool, bool, bool, error) {
	p.record("ProbeInfra", state)
	if err := p.evalFault(FaultStatusProberProbeInfra, ctx, state); err != nil {
		return false, false, false, err
	}
	if p.ProbeInfraErr != nil {
		if err := p.ProbeInfraErr(ctx, state); err != nil {
			return false, false, false, err
		}
	}
	return p.WG, p.DockerNet, p.Corrosion, nil
}
