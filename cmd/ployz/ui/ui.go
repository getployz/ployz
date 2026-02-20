package ui

import (
	"fmt"
	"strings"

	"github.com/charmbracelet/lipgloss"
	"github.com/charmbracelet/lipgloss/table"
)

// Palette — muted, professional, dark-terminal friendly.
var (
	purple = lipgloss.Color("99")
	green  = lipgloss.Color("76")
	red    = lipgloss.Color("204")
	yellow = lipgloss.Color("214")
	dim    = lipgloss.Color("243")
	faint  = lipgloss.Color("238")
)

// Base styles available for direct use.
var (
	AccentStyle  = lipgloss.NewStyle().Foreground(purple)
	SuccessStyle = lipgloss.NewStyle().Foreground(green)
	ErrorStyle   = lipgloss.NewStyle().Foreground(red)
	WarnStyle    = lipgloss.NewStyle().Foreground(yellow)
	MutedStyle   = lipgloss.NewStyle().Foreground(dim)
	FaintStyle   = lipgloss.NewStyle().Foreground(faint)
	BoldStyle    = lipgloss.NewStyle().Bold(true)
	LabelStyle   = lipgloss.NewStyle().Foreground(dim)
)

// Inline helpers — return styled text without newlines.

func Accent(s string) string  { return AccentStyle.Render(s) }
func Bold(s string) string    { return BoldStyle.Render(s) }
func Muted(s string) string   { return MutedStyle.Render(s) }
func Success(s string) string { return SuccessStyle.Render(s) }
func Warn(s string) string    { return WarnStyle.Render(s) }

func Bool(v bool) string {
	if v {
		return SuccessStyle.Render("true")
	}
	return ErrorStyle.Render("false")
}

// Message helpers — single-line strings (no trailing newline).

func SuccessMsg(format string, a ...any) string {
	return SuccessStyle.Render("✓") + " " + fmt.Sprintf(format, a...)
}

func WarnMsg(format string, a ...any) string {
	return WarnStyle.Render("!") + " " + fmt.Sprintf(format, a...)
}

func ErrorMsg(format string, a ...any) string {
	return ErrorStyle.Render("✗") + " " + fmt.Sprintf(format, a...)
}

func InfoMsg(format string, a ...any) string {
	return AccentStyle.Render("●") + " " + fmt.Sprintf(format, a...)
}

// Pair holds a key-value pair for KeyValues output.
// Fields are unexported; use KV to construct.
type Pair struct {
	key   string
	value string
}

// KV creates a key-value pair.
func KV(key, value string) Pair {
	return Pair{key: key, value: value}
}

// KeyValues renders aligned "key:  value" lines.
// Returns a multi-line string with trailing newline.
func KeyValues(indent string, pairs ...Pair) string {
	maxLen := 0
	for _, p := range pairs {
		if len(p.key) > maxLen {
			maxLen = len(p.key)
		}
	}

	var sb strings.Builder
	for _, p := range pairs {
		label := fmt.Sprintf("%-*s", maxLen+1, p.key+":")
		sb.WriteString(indent + LabelStyle.Render(label) + " " + p.value + "\n")
	}
	return sb.String()
}

// Table renders a styled table with rounded borders.
func Table(headers []string, rows [][]string) string {
	headerStyle := lipgloss.NewStyle().
		Foreground(purple).
		Bold(true).
		Padding(0, 1)

	cellStyle := lipgloss.NewStyle().Padding(0, 1)
	oddStyle := cellStyle.Foreground(dim)
	evenStyle := cellStyle

	t := table.New().
		Border(lipgloss.RoundedBorder()).
		BorderStyle(lipgloss.NewStyle().Foreground(faint)).
		StyleFunc(func(row, col int) lipgloss.Style {
			switch {
			case row == table.HeaderRow:
				return headerStyle
			case row%2 == 0:
				return evenStyle
			default:
				return oddStyle
			}
		}).
		Headers(headers...).
		Rows(rows...)

	return t.String()
}
