package ui

import (
	"fmt"
	"os"

	"github.com/charmbracelet/bubbles/table"
	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

// InteractiveTable renders a navigable table with keyboard controls.
// Returns the selected row index (0-based), or -1 if the user quits
// without selecting. In non-interactive mode the static Table is printed
// to stdout and -1 is returned.
func InteractiveTable(headers []string, rows [][]string) (int, error) {
	if IsNoInteraction() {
		fmt.Println(Table(headers, rows))
		return -1, nil
	}

	columns := make([]table.Column, len(headers))
	for i, h := range headers {
		w := len(h)
		for _, row := range rows {
			if i < len(row) && len(row[i]) > w {
				w = len(row[i])
			}
		}
		columns[i] = table.Column{Title: h, Width: w + 2}
	}

	tableRows := make([]table.Row, len(rows))
	for i, row := range rows {
		tableRows[i] = table.Row(row)
	}

	t := table.New(
		table.WithColumns(columns),
		table.WithRows(tableRows),
		table.WithFocused(true),
		table.WithHeight(min(len(rows), 20)),
	)

	s := table.DefaultStyles()
	s.Header = s.Header.
		Foreground(purple).
		Bold(true).
		BorderStyle(lipgloss.NormalBorder()).
		BorderBottom(true).
		BorderForeground(faint)
	s.Selected = s.Selected.
		Foreground(lipgloss.Color("229")).
		Background(purple).
		Bold(false)
	t.SetStyles(s)

	m := &itableModel{table: t, selected: -1}
	p := tea.NewProgram(m,
		tea.WithOutput(os.Stderr),
	)

	if _, err := p.Run(); err != nil {
		return -1, fmt.Errorf("interactive table: %w", err)
	}

	return m.selected, nil
}

type itableModel struct {
	table    table.Model
	selected int
}

func (m *itableModel) Init() tea.Cmd {
	return nil
}

func (m *itableModel) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.KeyMsg:
		switch msg.String() {
		case "q", "esc", "ctrl+c":
			m.selected = -1
			return m, tea.Quit
		case "enter":
			m.selected = m.table.Cursor()
			return m, tea.Quit
		}
	}

	var cmd tea.Cmd
	m.table, cmd = m.table.Update(msg)
	return m, cmd
}

func (m *itableModel) View() string {
	return m.table.View() + "\n" + MutedStyle.Render("↑/↓ navigate  enter select  q quit") + "\n"
}
