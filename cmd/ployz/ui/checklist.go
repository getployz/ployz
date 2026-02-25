package ui

import (
	"fmt"
	"os"
	"sync"
	"time"
)

var spinFrames = [...]string{"⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"}

// Checklist renders telemetry snapshots as a terminal checklist.
// Pending steps are muted, running steps show a braille spinner,
// done steps show a checkmark, failed steps show a red x.
type Checklist struct {
	steps         []stepState
	renderedLines int
	mu            sync.Mutex
	stop          chan struct{}
	frame         int
	once          sync.Once
}

// NewChecklist creates a Checklist ready to receive telemetry snapshots.
func NewChecklist() *Checklist {
	return &Checklist{stop: make(chan struct{})}
}

// OnSnapshot updates the checklist on each telemetry snapshot.
func (c *Checklist) OnSnapshot(snap stepSnapshot) {
	c.mu.Lock()
	defer c.mu.Unlock()

	first := c.steps == nil
	c.steps = snap.Steps

	if first {
		for _, s := range c.steps {
			icon, label := c.stepStyle(s)
			fmt.Fprintf(os.Stderr, "%s%s %s\n", stepIndent(s), icon, label)
		}
		c.renderedLines = len(c.steps)
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
	if len(c.steps) == 0 && c.renderedLines == 0 {
		return
	}
	if c.renderedLines > 0 {
		fmt.Fprintf(os.Stderr, "\033[%dA", c.renderedLines)
	}
	for _, s := range c.steps {
		icon, label := c.stepStyle(s)
		line := fmt.Sprintf("%s%s %s", stepIndent(s), icon, label)
		if s.Message != "" {
			line += " " + Muted(s.Message)
		}
		fmt.Fprintf(os.Stderr, "\r%s\033[K\n", line)
	}
	for i := len(c.steps); i < c.renderedLines; i++ {
		fmt.Fprint(os.Stderr, "\r\033[K\n")
	}
	c.renderedLines = len(c.steps)
}

func (c *Checklist) stepStyle(s stepState) (icon, label string) {
	switch s.Status {
	case stepRunning:
		return Accent(spinFrames[c.frame]), s.Title
	case stepDone:
		return Success("\u2713"), s.Title
	case stepFailed:
		return ErrorStyle.Render("\u2717"), ErrorStyle.Render(s.Title)
	default:
		return Muted("\u25cf"), Muted(s.Title)
	}
}

func stepIndent(s stepState) string {
	if s.ParentID != "" {
		return "    "
	}
	return "  "
}
