package ui

import (
	"testing"
)

func TestFormatStepLine(t *testing.T) {
	t.Parallel()

	testCases := []struct {
		name string
		step stepState
		msg  string
		want string
	}{
		{
			name: "running root",
			step: stepState{ID: "connect", Title: "connecting", Status: stepRunning},
			want: "  [->] connecting",
		},
		{
			name: "done child",
			step: stepState{ID: "add/install", ParentID: "add", Title: "install", Status: stepDone},
			want: "    [ok] install",
		},
		{
			name: "failed with message",
			step: stepState{ID: "save", Title: "saving", Status: stepFailed},
			msg:  "permission denied",
			want: "  [x] saving (permission denied)",
		},
	}

	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			got := formatStepLine(tc.step, tc.msg)
			if got != tc.want {
				t.Fatalf("formatStepLine() = %q, want %q", got, tc.want)
			}
		})
	}
}
