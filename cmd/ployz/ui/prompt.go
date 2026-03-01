package ui

import (
	"fmt"
	"os"
	"strings"

	"github.com/charmbracelet/bubbles/textinput"
	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

// Confirm asks the user a yes/no question on stderr and returns the answer.
// bypassHint describes how to skip the prompt in non-interactive mode (e.g.
// "use --yes to skip"). Non-interactive terminals return *ErrNoInteraction
// with the hint embedded.
func Confirm(question string, bypassHint string) (bool, error) {
	if err := RequireInteraction(bypassHint); err != nil {
		return false, fmt.Errorf("confirmation required: %w", err)
	}

	m := &confirmModel{question: question}
	p := tea.NewProgram(m,
		tea.WithOutput(os.Stderr),
	)

	if _, err := p.Run(); err != nil {
		return false, fmt.Errorf("confirm prompt: %w", err)
	}

	if m.cancelled {
		return false, ErrCancelled
	}
	return m.confirmed, nil
}

// Prompt asks the user for text input on stderr and returns the entered value.
// bypassHint describes how to provide the value non-interactively (e.g.
// "use --name <value>"). Non-interactive terminals return *ErrNoInteraction.
func Prompt(label, placeholder, bypassHint string) (string, error) {
	if err := RequireInteraction(bypassHint); err != nil {
		return "", fmt.Errorf("input required: %w", err)
	}

	ti := textinput.New()
	ti.Placeholder = placeholder
	ti.Focus()
	ti.PromptStyle = AccentStyle
	ti.TextStyle = lipgloss.NewStyle()

	m := &promptModel{
		label:     label,
		textInput: ti,
	}
	p := tea.NewProgram(m,
		tea.WithOutput(os.Stderr),
	)

	if _, err := p.Run(); err != nil {
		return "", fmt.Errorf("text prompt: %w", err)
	}

	if m.cancelled {
		return "", ErrCancelled
	}
	return m.textInput.Value(), nil
}

// confirmModel is a bubbletea model for yes/no confirmation.
type confirmModel struct {
	question  string
	confirmed bool
	cancelled bool
	answered  bool
}

func (m *confirmModel) Init() tea.Cmd { return nil }

func (m *confirmModel) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.KeyMsg:
		switch msg.String() {
		case "y", "Y":
			m.confirmed = true
			m.answered = true
			return m, tea.Quit
		case "n", "N", "enter":
			m.confirmed = false
			m.answered = true
			return m, tea.Quit
		case "ctrl+c", "esc":
			m.cancelled = true
			return m, tea.Quit
		}
	}
	return m, nil
}

func (m *confirmModel) View() string {
	if m.answered || m.cancelled {
		return ""
	}
	return AccentStyle.Render("?") + " " + m.question + " " + MutedStyle.Render("[y/N]") + " "
}

// promptModel is a bubbletea model for text input.
type promptModel struct {
	label     string
	textInput textinput.Model
	cancelled bool
	submitted bool
}

func (m *promptModel) Init() tea.Cmd {
	return textinput.Blink
}

func (m *promptModel) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.KeyMsg:
		switch msg.String() {
		case "enter":
			m.submitted = true
			return m, tea.Quit
		case "ctrl+c", "esc":
			m.cancelled = true
			return m, tea.Quit
		}
	}

	var cmd tea.Cmd
	m.textInput, cmd = m.textInput.Update(msg)
	return m, cmd
}

func (m *promptModel) View() string {
	if m.submitted || m.cancelled {
		return ""
	}

	var sb strings.Builder
	sb.WriteString(AccentStyle.Render("?") + " " + m.label + "\n")
	sb.WriteString(m.textInput.View() + "\n")
	return sb.String()
}
