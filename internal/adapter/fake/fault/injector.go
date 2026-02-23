package fault

import (
	"fmt"
	"strings"
	"sync"

	"ployz/internal/check"
)

type Hook func(args ...any) error

type pointFault struct {
	onceErrs  []error
	alwaysErr error
	hook      Hook
}

// Injector manages per-point fault injection for fake adapters.
// It supports one-shot failures, persistent failures, and argument-aware hooks.
type Injector struct {
	mu     sync.Mutex
	points map[string]*pointFault
}

func NewInjector() *Injector {
	return &Injector{points: make(map[string]*pointFault)}
}

// FailOnce injects err for the next evaluation of point.
func (i *Injector) FailOnce(point string, err error) {
	check.Assert(i != nil, "fault.Injector.FailOnce: receiver must not be nil")
	check.Assert(strings.TrimSpace(point) != "", "fault.Injector.FailOnce: point must not be empty")
	check.Assert(err != nil, "fault.Injector.FailOnce: err must not be nil")
	if i == nil || strings.TrimSpace(point) == "" || err == nil {
		return
	}

	i.mu.Lock()
	defer i.mu.Unlock()

	pf := i.ensurePoint(point)
	pf.onceErrs = append(pf.onceErrs, err)
}

// FailAlways injects err on every evaluation of point.
func (i *Injector) FailAlways(point string, err error) {
	check.Assert(i != nil, "fault.Injector.FailAlways: receiver must not be nil")
	check.Assert(strings.TrimSpace(point) != "", "fault.Injector.FailAlways: point must not be empty")
	check.Assert(err != nil, "fault.Injector.FailAlways: err must not be nil")
	if i == nil || strings.TrimSpace(point) == "" || err == nil {
		return
	}

	i.mu.Lock()
	defer i.mu.Unlock()

	pf := i.ensurePoint(point)
	pf.alwaysErr = err
}

// SetHook sets an argument-aware hook for point.
func (i *Injector) SetHook(point string, hook Hook) {
	check.Assert(i != nil, "fault.Injector.SetHook: receiver must not be nil")
	check.Assert(strings.TrimSpace(point) != "", "fault.Injector.SetHook: point must not be empty")
	check.Assert(hook != nil, "fault.Injector.SetHook: hook must not be nil")
	if i == nil || strings.TrimSpace(point) == "" || hook == nil {
		return
	}

	i.mu.Lock()
	defer i.mu.Unlock()

	pf := i.ensurePoint(point)
	pf.hook = hook
}

// Clear removes all faults for a single point.
func (i *Injector) Clear(point string) {
	check.Assert(i != nil, "fault.Injector.Clear: receiver must not be nil")
	check.Assert(strings.TrimSpace(point) != "", "fault.Injector.Clear: point must not be empty")
	if i == nil || strings.TrimSpace(point) == "" {
		return
	}

	i.mu.Lock()
	defer i.mu.Unlock()

	delete(i.points, point)
}

// Reset removes all configured faults.
func (i *Injector) Reset() {
	check.Assert(i != nil, "fault.Injector.Reset: receiver must not be nil")
	if i == nil {
		return
	}

	i.mu.Lock()
	i.points = make(map[string]*pointFault)
	i.mu.Unlock()
}

// Eval evaluates whether point should fail for this call.
// Precedence: hook -> once -> always.
func (i *Injector) Eval(point string, args ...any) error {
	check.Assert(i != nil, "fault.Injector.Eval: receiver must not be nil")
	check.Assert(strings.TrimSpace(point) != "", "fault.Injector.Eval: point must not be empty")
	if i == nil || strings.TrimSpace(point) == "" {
		return nil
	}

	i.mu.Lock()
	pf := i.points[point]
	if pf == nil {
		i.mu.Unlock()
		return nil
	}

	hook := pf.hook
	var onceErr error
	if len(pf.onceErrs) > 0 {
		onceErr = pf.onceErrs[0]
		pf.onceErrs = pf.onceErrs[1:]
	}
	alwaysErr := pf.alwaysErr
	i.mu.Unlock()

	if hook != nil {
		if err := hook(args...); err != nil {
			return fmt.Errorf("fault %s (hook): %w", point, err)
		}
	}
	if onceErr != nil {
		return fmt.Errorf("fault %s (once): %w", point, onceErr)
	}
	if alwaysErr != nil {
		return fmt.Errorf("fault %s (always): %w", point, alwaysErr)
	}

	return nil
}

func (i *Injector) ensurePoint(point string) *pointFault {
	pf, ok := i.points[point]
	if !ok {
		pf = &pointFault{}
		i.points[point] = pf
	}
	return pf
}
