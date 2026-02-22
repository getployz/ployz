package progress

import (
	"errors"
	"testing"
)

func TestDoEmitsSnapshotsInOrder(t *testing.T) {
	var snaps []Snapshot
	tr := New(func(s Snapshot) {
		snaps = append(snaps, s)
	},
		StepConfig{ID: "install", Title: "installing"},
		StepConfig{ID: "connect", Title: "connecting"},
	)

	if err := tr.Do("install", func() error { return nil }); err != nil {
		t.Fatalf("Do(install) error = %v", err)
	}
	if err := tr.Do("connect", func() error { return nil }); err != nil {
		t.Fatalf("Do(connect) error = %v", err)
	}

	// 1 initial + 2 per step (running, done) = 5
	if got, want := len(snaps), 5; got != want {
		t.Fatalf("snapshot count = %d, want %d", got, want)
	}

	assertStatuses(t, snaps[0], Pending, Pending)
	assertStatuses(t, snaps[1], Running, Pending)
	assertStatuses(t, snaps[2], Done, Pending)
	assertStatuses(t, snaps[3], Done, Running)
	assertStatuses(t, snaps[4], Done, Done)
}

func TestDoStopsOnError(t *testing.T) {
	wantErr := errors.New("boom")
	var snaps []Snapshot
	tr := New(func(s Snapshot) {
		snaps = append(snaps, s)
	},
		StepConfig{ID: "configure", Title: "configuring"},
		StepConfig{ID: "register", Title: "registering"},
	)

	err := tr.Do("configure", func() error { return wantErr })
	if !errors.Is(err, wantErr) {
		t.Fatalf("Do() error = %v, want %v", err, wantErr)
	}

	// 1 initial + 2 for the failed step (running, failed) = 3
	if got, want := len(snaps), 3; got != want {
		t.Fatalf("snapshot count = %d, want %d", got, want)
	}

	assertStatuses(t, snaps[0], Pending, Pending)
	assertStatuses(t, snaps[1], Running, Pending)
	assertStatuses(t, snaps[2], Failed, Pending)
	if got, want := snaps[2].Steps[0].Message, "boom"; got != want {
		t.Fatalf("failed message = %q, want %q", got, want)
	}
}

func TestDoUsesConfiguredTitles(t *testing.T) {
	var snap Snapshot
	tr := New(func(s Snapshot) { snap = s },
		StepConfig{
			ID:          "configure",
			Title:       "configuring network",
			DoneTitle:   "configured network",
			FailedTitle: "failed to configure network",
		},
	)

	if err := tr.Do("configure", func() error { return nil }); err != nil {
		t.Fatalf("Do() error = %v", err)
	}

	if got, want := snap.Steps[0].Status, Done; got != want {
		t.Fatalf("status = %q, want %q", got, want)
	}
	if got, want := snap.Steps[0].Title, "configured network"; got != want {
		t.Fatalf("title = %q, want %q", got, want)
	}
}

func TestStartMarksFailureWithFailedTitle(t *testing.T) {
	var snap Snapshot
	tr := New(func(s Snapshot) { snap = s },
		StepConfig{
			ID:          "connect",
			Title:       "connecting",
			FailedTitle: "failed to connect",
		},
	)

	end := tr.Start("connect")
	end(errors.New("dial tcp timeout"))

	if got, want := snap.Steps[0].Status, Failed; got != want {
		t.Fatalf("status = %q, want %q", got, want)
	}
	if got, want := snap.Steps[0].Title, "failed to connect"; got != want {
		t.Fatalf("title = %q, want %q", got, want)
	}
	if got, want := snap.Steps[0].Message, "dial tcp timeout"; got != want {
		t.Fatalf("message = %q, want %q", got, want)
	}
}

func assertStatuses(t *testing.T, snap Snapshot, statuses ...Status) {
	t.Helper()
	if got, want := len(snap.Steps), len(statuses); got != want {
		t.Fatalf("step count = %d, want %d", got, want)
	}
	for i, want := range statuses {
		if got := snap.Steps[i].Status; got != want {
			t.Fatalf("step %d status = %q, want %q", i, got, want)
		}
	}
}
