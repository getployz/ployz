package scenario

import (
	"context"
	"fmt"
	"math/rand"
	"sort"
	"strings"
	"sync"
	"time"

	"ployz/internal/check"
)

const (
	defaultChaosMaxEvents      = 4096
	defaultChaosOpWeight       = 1
	defaultChaosMaxAutoNodes   = 16
	defaultChaosAddNodeRetries = 3
)

// ChaosOperation mutates topology/state for one chaos step.
type ChaosOperation struct {
	Name   string
	Weight int
	Run    func(ctx context.Context, s *Scenario, rng *rand.Rand) (string, error)
}

// ChaosInvariant verifies a post-step invariant.
type ChaosInvariant struct {
	Name  string
	Check func(ctx context.Context, s *Scenario) error
}

// ChaosEvent records one executed step for replay/debugging.
type ChaosEvent struct {
	Step              int
	Seed              int64
	Timestamp         time.Time
	Operation         string
	Detail            string
	OperationError    string
	InvariantFailures []string
}

// ChaosRunnerConfig configures a ChaosRunner.
type ChaosRunnerConfig struct {
	Seed       int64
	MaxEvents  int
	Operations []ChaosOperation
	Invariants []ChaosInvariant
}

// ChaosRunner executes reproducible chaos steps and checks invariants.
type ChaosRunner struct {
	mu         sync.Mutex
	scenario   *Scenario
	rng        *rand.Rand
	seed       int64
	step       int
	maxEvents  int
	operations []ChaosOperation
	invariants []ChaosInvariant
	events     []ChaosEvent
}

func NewChaosRunner(s *Scenario, cfg ChaosRunnerConfig) (*ChaosRunner, error) {
	check.Assert(s != nil, "NewChaosRunner: scenario must not be nil")
	if s == nil {
		return nil, fmt.Errorf("scenario is required")
	}

	seed := cfg.Seed
	if seed == 0 {
		seed = time.Now().UnixNano()
	}

	maxEvents := cfg.MaxEvents
	if maxEvents <= 0 {
		maxEvents = defaultChaosMaxEvents
	}

	ops := cfg.Operations
	if len(ops) == 0 {
		ops = DefaultChaosOperations()
	}
	for _, op := range ops {
		if strings.TrimSpace(op.Name) == "" {
			return nil, fmt.Errorf("chaos operation name is required")
		}
		if op.Run == nil {
			return nil, fmt.Errorf("chaos operation %q run func is required", op.Name)
		}
	}

	invariants := cfg.Invariants
	if len(invariants) == 0 {
		invariants = DefaultChaosInvariants()
	}
	for _, inv := range invariants {
		if strings.TrimSpace(inv.Name) == "" {
			return nil, fmt.Errorf("chaos invariant name is required")
		}
		if inv.Check == nil {
			return nil, fmt.Errorf("chaos invariant %q check func is required", inv.Name)
		}
	}

	return &ChaosRunner{
		scenario:   s,
		rng:        rand.New(rand.NewSource(seed)),
		seed:       seed,
		maxEvents:  maxEvents,
		operations: append([]ChaosOperation(nil), ops...),
		invariants: append([]ChaosInvariant(nil), invariants...),
		events:     make([]ChaosEvent, 0, min(maxEvents, 128)),
	}, nil
}

func (r *ChaosRunner) Seed() int64 {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.seed
}

func (r *ChaosRunner) RegisterOperation(op ChaosOperation) error {
	if strings.TrimSpace(op.Name) == "" {
		return fmt.Errorf("chaos operation name is required")
	}
	if op.Run == nil {
		return fmt.Errorf("chaos operation %q run func is required", op.Name)
	}

	r.mu.Lock()
	r.operations = append(r.operations, op)
	r.mu.Unlock()
	return nil
}

func (r *ChaosRunner) RegisterInvariant(inv ChaosInvariant) error {
	if strings.TrimSpace(inv.Name) == "" {
		return fmt.Errorf("chaos invariant name is required")
	}
	if inv.Check == nil {
		return fmt.Errorf("chaos invariant %q check func is required", inv.Name)
	}

	r.mu.Lock()
	r.invariants = append(r.invariants, inv)
	r.mu.Unlock()
	return nil
}

func (r *ChaosRunner) ReplayLog() []ChaosEvent {
	r.mu.Lock()
	defer r.mu.Unlock()

	out := make([]ChaosEvent, len(r.events))
	for i := range r.events {
		out[i] = ChaosEvent{
			Step:           r.events[i].Step,
			Seed:           r.events[i].Seed,
			Timestamp:      r.events[i].Timestamp,
			Operation:      r.events[i].Operation,
			Detail:         r.events[i].Detail,
			OperationError: r.events[i].OperationError,
		}
		if len(r.events[i].InvariantFailures) > 0 {
			out[i].InvariantFailures = append([]string(nil), r.events[i].InvariantFailures...)
		}
	}
	return out
}

func (r *ChaosRunner) Step(ctx context.Context) error {
	if err := ctx.Err(); err != nil {
		return err
	}

	r.mu.Lock()
	op, err := chooseChaosOperation(r.rng, r.operations)
	if err != nil {
		r.mu.Unlock()
		return err
	}
	r.step++
	step := r.step
	seed := r.seed
	r.mu.Unlock()

	detail, opErr := op.Run(ctx, r.scenario, r.rng)
	invFailures := r.checkInvariants(ctx)

	event := ChaosEvent{
		Step:              step,
		Seed:              seed,
		Timestamp:         runnerNow(r.scenario),
		Operation:         op.Name,
		Detail:            detail,
		InvariantFailures: invFailures,
	}
	if opErr != nil {
		event.OperationError = opErr.Error()
	}
	r.appendEvent(event)

	if opErr != nil {
		return fmt.Errorf("chaos step %d op %q: %w", step, op.Name, opErr)
	}
	if len(invFailures) > 0 {
		return fmt.Errorf("chaos step %d invariant failures: %s", step, strings.Join(invFailures, "; "))
	}
	return nil
}

func (r *ChaosRunner) Run(ctx context.Context, steps int) error {
	if steps <= 0 {
		return fmt.Errorf("steps must be > 0")
	}

	for i := 0; i < steps; i++ {
		if err := ctx.Err(); err != nil {
			return err
		}
		if err := r.Step(ctx); err != nil {
			return err
		}
	}
	return nil
}

func (r *ChaosRunner) checkInvariants(ctx context.Context) []string {
	r.mu.Lock()
	invariants := append([]ChaosInvariant(nil), r.invariants...)
	r.mu.Unlock()

	failures := make([]string, 0)
	for _, inv := range invariants {
		if err := inv.Check(ctx, r.scenario); err != nil {
			failures = append(failures, fmt.Sprintf("%s: %v", inv.Name, err))
		}
	}
	sort.Strings(failures)
	return failures
}

func (r *ChaosRunner) appendEvent(event ChaosEvent) {
	r.mu.Lock()
	defer r.mu.Unlock()

	r.events = append(r.events, event)
	if len(r.events) > r.maxEvents {
		r.events = r.events[len(r.events)-r.maxEvents:]
	}
}

func runnerNow(s *Scenario) time.Time {
	if s != nil && s.clock != nil {
		return s.clock.Now()
	}
	return time.Now()
}

func chooseChaosOperation(rng *rand.Rand, ops []ChaosOperation) (ChaosOperation, error) {
	total := 0
	for _, op := range ops {
		w := op.Weight
		if w <= 0 {
			w = defaultChaosOpWeight
		}
		total += w
	}
	if total <= 0 {
		return ChaosOperation{}, fmt.Errorf("no chaos operations registered")
	}

	pick := rng.Intn(total)
	for _, op := range ops {
		w := op.Weight
		if w <= 0 {
			w = defaultChaosOpWeight
		}
		if pick < w {
			return op, nil
		}
		pick -= w
	}

	return ChaosOperation{}, fmt.Errorf("failed to choose chaos operation")
}

func DefaultChaosOperations() []ChaosOperation {
	return []ChaosOperation{
		{
			Name:   "heal",
			Weight: 1,
			Run: func(ctx context.Context, s *Scenario, rng *rand.Rand) (string, error) {
				s.Heal()
				return "all partitions healed", nil
			},
		},
		{
			Name:   "partition_pair",
			Weight: 2,
			Run: func(ctx context.Context, s *Scenario, rng *rand.Rand) (string, error) {
				ids := s.Nodes()
				if len(ids) < 2 {
					return "skip: need at least 2 nodes", nil
				}
				a := rng.Intn(len(ids))
				b := rng.Intn(len(ids) - 1)
				if b >= a {
					b++
				}
				s.Partition([]string{ids[a]}, []string{ids[b]})
				return fmt.Sprintf("partitioned %s <-> %s", ids[a], ids[b]), nil
			},
		},
		{
			Name:   "kill_node",
			Weight: 2,
			Run: func(ctx context.Context, s *Scenario, rng *rand.Rand) (string, error) {
				ids := s.Nodes()
				if len(ids) == 0 {
					return "skip: no nodes", nil
				}
				id := ids[rng.Intn(len(ids))]
				s.KillNode(id)
				return fmt.Sprintf("killed %s", id), nil
			},
		},
		{
			Name:   "restart_node",
			Weight: 2,
			Run: func(ctx context.Context, s *Scenario, rng *rand.Rand) (string, error) {
				ids := s.Nodes()
				if len(ids) == 0 {
					return "skip: no nodes", nil
				}
				id := ids[rng.Intn(len(ids))]
				s.RestartNode(id)
				return fmt.Sprintf("restarted %s", id), nil
			},
		},
		{
			Name:   "add_node",
			Weight: 1,
			Run: func(ctx context.Context, s *Scenario, rng *rand.Rand) (string, error) {
				if len(s.Nodes()) >= defaultChaosMaxAutoNodes {
					return fmt.Sprintf("skip: max auto nodes (%d)", defaultChaosMaxAutoNodes), nil
				}
				for attempt := 0; attempt < defaultChaosAddNodeRetries; attempt++ {
					id := fmt.Sprintf("chaos-%04d", rng.Intn(defaultChaosMaxAutoNodes*100))
					if s.Node(id) != nil {
						continue
					}
					if _, err := s.AddNode(id); err != nil {
						return "", err
					}
					return fmt.Sprintf("added %s", id), nil
				}
				return "skip: no unique node id", nil
			},
		},
		{
			Name:   "remove_node",
			Weight: 1,
			Run: func(ctx context.Context, s *Scenario, rng *rand.Rand) (string, error) {
				ids := s.Nodes()
				if len(ids) <= 1 {
					return "skip: keep at least one node", nil
				}
				id := ids[rng.Intn(len(ids))]
				if err := s.RemoveNode(id); err != nil {
					return "", err
				}
				return fmt.Sprintf("removed %s", id), nil
			},
		},
		{
			Name:   "tick_or_drain",
			Weight: 3,
			Run: func(ctx context.Context, s *Scenario, rng *rand.Rand) (string, error) {
				if rng.Intn(2) == 0 {
					s.Tick()
					return "tick", nil
				}
				s.Drain()
				return "drain", nil
			},
		},
	}
}

func DefaultChaosInvariants() []ChaosInvariant {
	return []ChaosInvariant{
		{
			Name: "node_ids_unique",
			Check: func(ctx context.Context, s *Scenario) error {
				ids := s.Nodes()
				seen := make(map[string]struct{}, len(ids))
				for _, id := range ids {
					if strings.TrimSpace(id) == "" {
						return fmt.Errorf("empty node id")
					}
					if _, exists := seen[id]; exists {
						return fmt.Errorf("duplicate node id %q", id)
					}
					seen[id] = struct{}{}
				}
				return nil
			},
		},
		{
			Name: "snapshot_readable",
			Check: func(ctx context.Context, s *Scenario) error {
				for _, id := range s.Nodes() {
					_ = s.Snapshot(id)
				}
				return nil
			},
		},
		{
			Name: "machine_versions_positive",
			Check: func(ctx context.Context, s *Scenario) error {
				for _, id := range s.Nodes() {
					snap := s.Snapshot(id)
					for _, row := range snap.Machines {
						if row.Version <= 0 {
							return fmt.Errorf("node %s machine %s has non-positive version %d", id, row.ID, row.Version)
						}
					}
				}
				return nil
			},
		},
	}
}

func min(a, b int) int {
	if a < b {
		return a
	}
	return b
}
