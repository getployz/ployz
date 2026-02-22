package ui

import (
	"fmt"
	"os"
	"sync"
	"time"

	"ployz/pkg/sdk/progress"
)

var spinFrames = [...]string{"⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"}

// Checklist renders SDK progress snapshots as a terminal checklist.
// Pending steps are muted, running steps show a braille spinner,
// done steps show a checkmark, failed steps show a red x.
type Checklist struct {
	steps []progress.Step
	mu    sync.Mutex
	stop  chan struct{}
	frame int
	once  sync.Once
}

// NewChecklist creates a Checklist ready to receive progress snapshots.
func NewChecklist() *Checklist {
	return &Checklist{stop: make(chan struct{})}
}

// OnProgress is a progress.Reporter that updates the checklist on each snapshot.
func (c *Checklist) OnProgress(snap progress.Snapshot) {
	c.mu.Lock()
	defer c.mu.Unlock()

	first := c.steps == nil
	c.steps = snap.Steps

	if first {
		for _, s := range c.steps {
			icon, label := c.stepStyle(s)
			fmt.Fprintf(os.Stderr, "%s%s %s\n", stepIndent(s), icon, label)
		}
		go c.spin()
		return
	}
	c.redraw()
}

// Close stops the spinner.
func (c *Checklist) Close() {
	c.once.Do(func() {
		close(c.stop)
	})
}

func (c *Checklist) spin() {
	ticker := time.NewTicker(80 * time.Millisecond)
	defer ticker.Stop()
	for {
		select {
		case <-c.stop:
			return
		case <-ticker.C:
			c.mu.Lock()
			c.frame = (c.frame + 1) % len(spinFrames)
			c.redraw()
			c.mu.Unlock()
		}
	}
}

// redraw reprints all step lines in place. Caller must hold c.mu.
func (c *Checklist) redraw() {
	n := len(c.steps)
	if n == 0 {
		return
	}
	fmt.Fprintf(os.Stderr, "\033[%dA", n)
	for _, s := range c.steps {
		icon, label := c.stepStyle(s)
		line := fmt.Sprintf("%s%s %s", stepIndent(s), icon, label)
		if s.Message != "" {
			line += " " + Muted(s.Message)
		}
		fmt.Fprintf(os.Stderr, "\r%s\033[K\n", line)
	}
}

func (c *Checklist) stepStyle(s progress.Step) (icon, label string) {
	switch s.Status {
	case progress.Running:
		return Accent(spinFrames[c.frame]), s.Title
	case progress.Done:
		return Success("\u2713"), s.Title
	case progress.Failed:
		return ErrorStyle.Render("\u2717"), ErrorStyle.Render(s.Title)
	default:
		return Muted("\u25cf"), Muted(s.Title)
	}
}

func stepIndent(s progress.Step) string {
	if s.ParentID != "" {
		return "    "
	}
	return "  "
}
