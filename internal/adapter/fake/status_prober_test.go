package fake

import (
	"context"
	"errors"
	"testing"

	"ployz/internal/mesh"
)

func TestStatusProber_CannedValues(t *testing.T) {
	ctx := t.Context()
	p := &StatusProber{WG: true, DockerNet: true, Corrosion: false}

	wg, dn, cr, err := p.ProbeInfra(ctx, &mesh.State{})
	if err != nil {
		t.Fatal(err)
	}
	if !wg || !dn || cr {
		t.Errorf("expected wg=true docker=true corrosion=false, got wg=%v docker=%v corrosion=%v", wg, dn, cr)
	}
}

func TestStatusProber_ErrorInjection(t *testing.T) {
	ctx := t.Context()
	injected := errors.New("probe failed")
	p := &StatusProber{
		WG:            true,
		ProbeInfraErr: func(context.Context, *mesh.State) error { return injected },
	}

	_, _, _, err := p.ProbeInfra(ctx, &mesh.State{})
	if !errors.Is(err, injected) {
		t.Errorf("expected injected error, got %v", err)
	}
}

func TestStatusProber_CallRecording(t *testing.T) {
	ctx := t.Context()
	p := &StatusProber{}
	_, _, _, _ = p.ProbeInfra(ctx, nil)
	_, _, _, _ = p.ProbeInfra(ctx, &mesh.State{})

	if len(p.Calls("ProbeInfra")) != 2 {
		t.Errorf("expected 2 ProbeInfra calls, got %d", len(p.Calls("ProbeInfra")))
	}
}

func TestStatusProber_FaultFailOnce(t *testing.T) {
	ctx := t.Context()
	p := &StatusProber{WG: true, DockerNet: true, Corrosion: true}
	injected := errors.New("injected")
	p.FailOnce(FaultStatusProberProbeInfra, injected)

	_, _, _, err := p.ProbeInfra(ctx, &mesh.State{})
	if !errors.Is(err, injected) {
		t.Fatalf("first ProbeInfra() error = %v, want injected", err)
	}

	wg, dn, cr, err := p.ProbeInfra(ctx, &mesh.State{})
	if err != nil {
		t.Fatalf("second ProbeInfra() error = %v, want nil", err)
	}
	if !wg || !dn || !cr {
		t.Fatalf("second ProbeInfra() values = (%v,%v,%v), want all true", wg, dn, cr)
	}
}
