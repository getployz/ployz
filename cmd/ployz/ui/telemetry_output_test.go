package ui

import (
	"testing"

	"ployz/pkg/sdk/telemetry"
)

func TestStepObserverFanoutCountersForPlannedParent(t *testing.T) {
	t.Parallel()

	snapshots := make([]stepSnapshot, 0, 8)
	observer := newStepObserver(func(snapshot stepSnapshot) {
		copied := stepSnapshot{Steps: append([]stepState(nil), snapshot.Steps...)}
		snapshots = append(snapshots, copied)
	})

	observer.onPlan(telemetry.Plan{Steps: []telemetry.PlannedStep{
		{ID: "remote", Title: "tearing down remote machines"},
		{ID: "local", Title: "tearing down local"},
	}})
	observer.onStepStart("remote")
	observer.onStepStart("remote/node-a")
	observer.onStepEnd("remote/node-a", false, "")
	observer.onStepStart("remote/node-b")
	observer.onStepEnd("remote/node-b", false, "")
	observer.onStepEnd("remote", false, "")

	if len(snapshots) == 0 {
		t.Fatal("expected telemetry snapshots")
	}

	final := snapshots[len(snapshots)-1]
	parent, ok := stepByID(final, "remote")
	if !ok {
		t.Fatal("missing parent step remote")
	}
	if parent.Status != stepDone {
		t.Fatalf("parent status = %q, want done", parent.Status)
	}
	if parent.Message != "2/2 done" {
		t.Fatalf("parent message = %q, want 2/2 done", parent.Message)
	}
}

func TestStepObserverCreatesSyntheticParentForDynamicChildren(t *testing.T) {
	t.Parallel()

	snapshots := make([]stepSnapshot, 0, 4)
	observer := newStepObserver(func(snapshot stepSnapshot) {
		copied := stepSnapshot{Steps: append([]stepState(nil), snapshot.Steps...)}
		snapshots = append(snapshots, copied)
	})

	observer.onStepStart("contact/node-1")
	observer.onStepEnd("contact/node-1", false, "")

	if len(snapshots) == 0 {
		t.Fatal("expected telemetry snapshots")
	}

	final := snapshots[len(snapshots)-1]
	parent, ok := stepByID(final, "contact")
	if !ok {
		t.Fatal("missing synthetic parent step")
	}
	if parent.Status != stepDone {
		t.Fatalf("synthetic parent status = %q, want done", parent.Status)
	}
	if parent.Message != "1/1 done" {
		t.Fatalf("synthetic parent message = %q, want 1/1 done", parent.Message)
	}

	child, ok := stepByID(final, "contact/node-1")
	if !ok {
		t.Fatal("missing child step")
	}
	if child.ParentID != "contact" {
		t.Fatalf("child parent id = %q, want contact", child.ParentID)
	}
}

func TestStepObserverKeepsFanoutCountersOnParentFailure(t *testing.T) {
	t.Parallel()

	snapshots := make([]stepSnapshot, 0, 6)
	observer := newStepObserver(func(snapshot stepSnapshot) {
		copied := stepSnapshot{Steps: append([]stepState(nil), snapshot.Steps...)}
		snapshots = append(snapshots, copied)
	})

	observer.onPlan(telemetry.Plan{Steps: []telemetry.PlannedStep{{
		ID:    "remote",
		Title: "tearing down remote machines",
	}}})
	observer.onStepStart("remote")
	observer.onStepStart("remote/node-1")
	observer.onStepEnd("remote/node-1", true, "timeout")
	observer.onStepEnd("remote", true, "remote teardown failed")

	if len(snapshots) == 0 {
		t.Fatal("expected telemetry snapshots")
	}

	final := snapshots[len(snapshots)-1]
	parent, ok := stepByID(final, "remote")
	if !ok {
		t.Fatal("missing parent step remote")
	}
	if parent.Status != stepFailed {
		t.Fatalf("parent status = %q, want failed", parent.Status)
	}
	if parent.Message != "0/1 done, 1 failed; remote teardown failed" {
		t.Fatalf("parent message = %q", parent.Message)
	}
}

func stepByID(snapshot stepSnapshot, id string) (stepState, bool) {
	for _, step := range snapshot.Steps {
		if step.ID == id {
			return step, true
		}
	}
	return stepState{}, false
}
