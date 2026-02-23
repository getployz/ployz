package scenario

import (
	"context"
	"errors"
	"math/rand"
	"testing"

	"ployz/internal/mesh"
)

func TestChaosRunnerStepRecordsReplayEvent(t *testing.T) {
	ctx := t.Context()
	s := MustNew(t, ctx, Config{NodeIDs: []string{"a", "b"}})

	r, err := NewChaosRunner(s, ChaosRunnerConfig{
		Seed: 42,
		Operations: []ChaosOperation{
			{
				Name:   "noop",
				Weight: 1,
				Run: func(ctx context.Context, s *Scenario, rng *rand.Rand) (string, error) {
					return "ok", nil
				},
			},
		},
		Invariants: []ChaosInvariant{{
			Name: "always_ok",
			Check: func(ctx context.Context, s *Scenario) error {
				return nil
			},
		}},
	})
	if err != nil {
		t.Fatalf("NewChaosRunner: %v", err)
	}

	if err := r.Step(ctx); err != nil {
		t.Fatalf("Step: %v", err)
	}

	log := r.ReplayLog()
	if len(log) != 1 {
		t.Fatalf("ReplayLog len: got %d want 1", len(log))
	}
	if log[0].Operation != "noop" {
		t.Fatalf("operation: got %q want %q", log[0].Operation, "noop")
	}
	if log[0].Step != 1 {
		t.Fatalf("step: got %d want 1", log[0].Step)
	}
}

func TestChaosRunnerRunBounded(t *testing.T) {
	ctx := t.Context()
	s := MustNew(t, ctx, Config{NodeIDs: []string{"a"}})

	count := 0
	r, err := NewChaosRunner(s, ChaosRunnerConfig{
		Seed: 7,
		Operations: []ChaosOperation{{
			Name: "count",
			Run: func(ctx context.Context, s *Scenario, rng *rand.Rand) (string, error) {
				count++
				return "count", nil
			},
		}},
		Invariants: []ChaosInvariant{{
			Name: "ok",
			Check: func(ctx context.Context, s *Scenario) error {
				return nil
			},
		}},
	})
	if err != nil {
		t.Fatalf("NewChaosRunner: %v", err)
	}

	if err := r.Run(ctx, 5); err != nil {
		t.Fatalf("Run: %v", err)
	}
	if count != 5 {
		t.Fatalf("operation count: got %d want 5", count)
	}
	if len(r.ReplayLog()) != 5 {
		t.Fatalf("ReplayLog len: got %d want 5", len(r.ReplayLog()))
	}
}

func TestChaosRunnerInvariantFailure(t *testing.T) {
	ctx := t.Context()
	s := MustNew(t, ctx, Config{NodeIDs: []string{"a"}})

	failErr := errors.New("invariant failure")
	r, err := NewChaosRunner(s, ChaosRunnerConfig{
		Seed: 9,
		Operations: []ChaosOperation{{
			Name: "noop",
			Run: func(ctx context.Context, s *Scenario, rng *rand.Rand) (string, error) {
				return "noop", nil
			},
		}},
		Invariants: []ChaosInvariant{{
			Name: "fail",
			Check: func(ctx context.Context, s *Scenario) error {
				return failErr
			},
		}},
	})
	if err != nil {
		t.Fatalf("NewChaosRunner: %v", err)
	}

	err = r.Step(ctx)
	if err == nil {
		t.Fatal("expected Step to fail on invariant")
	}

	log := r.ReplayLog()
	if len(log) != 1 {
		t.Fatalf("ReplayLog len: got %d want 1", len(log))
	}
	if len(log[0].InvariantFailures) != 1 {
		t.Fatalf("invariant failures len: got %d want 1", len(log[0].InvariantFailures))
	}
}

func TestChaosRunnerDefaultOperations(t *testing.T) {
	ctx := t.Context()
	clk := mesh.RealClock{}
	s := MustNew(t, ctx, Config{NodeIDs: []string{"a", "b", "c"}, Clock: clk})

	r, err := NewChaosRunner(s, ChaosRunnerConfig{Seed: 123})
	if err != nil {
		t.Fatalf("NewChaosRunner: %v", err)
	}

	if err := r.Run(ctx, 20); err != nil {
		t.Fatalf("Run default operations: %v", err)
	}
	if len(r.ReplayLog()) != 20 {
		t.Fatalf("ReplayLog len: got %d want 20", len(r.ReplayLog()))
	}
}
