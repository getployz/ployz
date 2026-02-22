package progress

import (
	"strings"
	"sync"
)

// Status represents the lifecycle state of a step.
type Status string

const (
	Pending Status = "pending"
	Running Status = "running"
	Done    Status = "done"
	Failed  Status = "failed"
)

// Step describes a unit of work within a long-running operation.
type Step struct {
	ID       string
	ParentID string // empty for top-level steps
	Title    string
	Message  string // optional detail
	Status   Status
}

// StepConfig configures titles and metadata for a tracked step.
type StepConfig struct {
	ID          string
	ParentID    string
	Title       string
	DoneTitle   string
	FailedTitle string
	Message     string
}

// Snapshot is the full state of all steps, emitted on every change.
type Snapshot struct {
	Steps []Step
}

// Reporter receives a snapshot whenever any step transitions.
type Reporter func(Snapshot)

// Tracker manages a list of steps and emits snapshots on every state change.
// It is the standard way for SDK operations to report progress.
type Tracker struct {
	mu       sync.Mutex
	steps    []Step
	configs  map[string]StepConfig
	stepByID map[string]int
	reporter Reporter
}

// New creates a tracker using static step configuration.
func New(reporter Reporter, steps ...StepConfig) *Tracker {
	t := &Tracker{
		steps:    make([]Step, 0, len(steps)),
		configs:  make(map[string]StepConfig, len(steps)),
		stepByID: make(map[string]int, len(steps)),
		reporter: reporter,
	}

	for _, cfg := range steps {
		t.addStepLocked(cfg)
	}

	t.emitLocked()
	return t
}

// Start transitions a step to Running and returns an end handle.
// Call the returned function with nil on success or with an error on failure.
func (t *Tracker) Start(id string) func(error) {
	id = normalizeID(id)

	t.mu.Lock()
	idx, cfg := t.ensureStepLocked(id)
	t.steps[idx].Status = Running
	t.steps[idx].Title = cfg.baseTitle()
	t.steps[idx].Message = cfg.Message
	t.emitLocked()
	t.mu.Unlock()

	var once sync.Once
	return func(err error) {
		once.Do(func() {
			t.finish(id, err)
		})
	}
}

// Do is sugar for Start + fn + end(err).
func (t *Tracker) Do(id string, fn func() error) error {
	end := t.Start(id)
	if fn == nil {
		end(nil)
		return nil
	}
	err := fn()
	end(err)
	return err
}

func (t *Tracker) finish(id string, err error) {
	t.mu.Lock()
	defer t.mu.Unlock()

	idx, cfg := t.ensureStepLocked(id)
	if err != nil {
		t.steps[idx].Status = Failed
		t.steps[idx].Title = cfg.failedTitle()
		t.steps[idx].Message = strings.TrimSpace(err.Error())
		if t.steps[idx].Message == "" {
			t.steps[idx].Message = cfg.Message
		}
		t.emitLocked()
		return
	}

	t.steps[idx].Status = Done
	t.steps[idx].Title = cfg.doneTitle()
	t.steps[idx].Message = cfg.Message
	t.emitLocked()
}

func (t *Tracker) addStepLocked(cfg StepConfig) {
	id := normalizeID(cfg.ID)
	if id == "" {
		return
	}

	cfg.ID = id
	t.configs[id] = cfg
	if _, exists := t.stepByID[id]; exists {
		return
	}

	t.stepByID[id] = len(t.steps)
	t.steps = append(t.steps, Step{
		ID:       id,
		ParentID: cfg.ParentID,
		Title:    cfg.baseTitle(),
		Message:  cfg.Message,
		Status:   Pending,
	})
}

func (t *Tracker) ensureStepLocked(id string) (int, StepConfig) {
	id = normalizeID(id)
	if idx, ok := t.stepByID[id]; ok {
		cfg, ok := t.configs[id]
		if !ok {
			cfg = StepConfig{ID: id, Title: id}
			t.configs[id] = cfg
		}
		return idx, cfg
	}

	cfg := StepConfig{ID: id, Title: id}
	t.addStepLocked(cfg)
	return t.stepByID[id], cfg
}

func (t *Tracker) emitLocked() {
	if t.reporter == nil {
		return
	}

	snap := make([]Step, len(t.steps))
	copy(snap, t.steps)
	t.reporter(Snapshot{Steps: snap})
}

func normalizeID(id string) string {
	id = strings.TrimSpace(id)
	if id == "" {
		return "unnamed"
	}
	return id
}

func (c StepConfig) baseTitle() string {
	if strings.TrimSpace(c.Title) != "" {
		return c.Title
	}
	return c.ID
}

func (c StepConfig) doneTitle() string {
	if strings.TrimSpace(c.DoneTitle) != "" {
		return c.DoneTitle
	}
	return c.baseTitle()
}

func (c StepConfig) failedTitle() string {
	if strings.TrimSpace(c.FailedTitle) != "" {
		return c.FailedTitle
	}
	return c.baseTitle()
}
