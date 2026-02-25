package cmdutil

import (
	"fmt"
	"io"
	"strings"

	"ployz/cmd/ployz/ui"
	"ployz/pkg/sdk/types"
)

type IssueLevel uint8

const (
	IssueLevelBlocker IssueLevel = iota
	IssueLevelWarning
)

func PrintStatusIssues(out io.Writer, issues []types.StatusIssue, level IssueLevel) {
	for _, issue := range issues {
		phase := strings.TrimSpace(issue.Phase)
		if phase == "" {
			phase = "unknown"
		}

		message := "%s (%s): %s"
		switch level {
		case IssueLevelWarning:
			fmt.Fprintln(out, ui.WarnMsg(message, issue.Component, phase, issue.Message))
		default:
			fmt.Fprintln(out, ui.ErrorMsg(message, issue.Component, phase, issue.Message))
		}

		if hint := strings.TrimSpace(issue.Hint); hint != "" {
			fmt.Fprintln(out, ui.Muted("  fix: "+hint))
		}
	}
}
