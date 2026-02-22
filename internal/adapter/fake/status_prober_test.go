package fake

import (
	"context"
	"errors"
	"testing"

	"ployz/internal/network"
)

func TestStatusProber_CannedValues(t *testing.T) {
	ctx := context.Background()
	p := &StatusProber{WG: true, DockerNet: true, Corrosion: false}

	wg, dn, cr, err := p.ProbeInfra(ctx, &network.State{})
	if err != nil {
		t.Fatal(err)
	}
	if !wg || !dn || cr {
		t.Errorf("expected wg=true docker=true corrosion=false, got wg=%v docker=%v corrosion=%v", wg, dn, cr)
	}
}

func TestStatusProber_ErrorInjection(t *testing.T) {
	ctx := context.Background()
	injected := errors.New("probe failed")
	p := &StatusProber{
		WG:            true,
		ProbeInfraErr: func(context.Context, *network.State) error { return injected },
	}

	_, _, _, err := p.ProbeInfra(ctx, &network.State{})
	if !errors.Is(err, injected) {
		t.Errorf("expected injected error, got %v", err)
	}
}

func TestStatusProber_CallRecording(t *testing.T) {
	ctx := context.Background()
	p := &StatusProber{}
	_, _, _, _ = p.ProbeInfra(ctx, nil)
	_, _, _, _ = p.ProbeInfra(ctx, &network.State{})

	if len(p.Calls("ProbeInfra")) != 2 {
		t.Errorf("expected 2 ProbeInfra calls, got %d", len(p.Calls("ProbeInfra")))
	}
}
