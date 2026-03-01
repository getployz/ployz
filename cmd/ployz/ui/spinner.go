package ui

import (
	"context"
	"fmt"
	"os"

	"github.com/charmbracelet/bubbles/spinner"
	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

// RunWithSpinner runs fn while displaying an animated spinner on stderr.
// In non-interactive mode the function runs synchronously with no output.
// Ctrl+C cancels the context passed to fn.
func RunWithSpinner(ctx context.Context, msg string, fn func(ctx context.Context) error) error {
	if IsNoInteraction() {
		return fn(ctx)
	}

	m := &spinnerModel{
		spinner: spinner.New(
			spinner.WithSpinner(spinner.MiniDot),
			spinner.WithStyle(lipgloss.NewStyle().Foreground(purple)),
		),
		msg: msg,
	}

	fnCtx, fnCancel := context.WithCancel(ctx)
	defer fnCancel()

	p := tea.NewProgram(m,
		tea.WithOutput(os.Stderr),
		tea.WithContext(ctx),
	)

	go func() {
		m.err = fn(fnCtx)
		p.Send(spinnerDoneMsg{})
	}()

	if _, err := p.Run(); err != nil {
		return fmt.Errorf("spinner: %w", err)
	}

	if m.cancelled {
		fnCancel()
		return context.Canceled
	}

	return m.err
}

type spinnerDoneMsg struct{}

type spinnerModel struct {
	spinner   spinner.Model
	msg       string
	err       error
	done      bool
	cancelled bool
}

func (m *spinnerModel) Init() tea.Cmd {
	return m.spinner.Tick
}

func (m *spinnerModel) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.KeyMsg:
		switch msg.String() {
		case "ctrl+c":
			m.cancelled = true
			return m, tea.Quit
		}
	case spinnerDoneMsg:
		m.done = true
		return m, tea.Quit
	case spinner.TickMsg:
		var cmd tea.Cmd
		m.spinner, cmd = m.spinner.Update(msg)
		return m, cmd
	}
	return m, nil
}

func (m *spinnerModel) View() string {
	if m.done || m.cancelled {
		return ""
	}
	return m.spinner.View() + " " + m.msg + "\n"
}
