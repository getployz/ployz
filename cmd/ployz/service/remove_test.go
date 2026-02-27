package service

import (
	"slices"
	"testing"
)

func TestRemoveCmdShape(t *testing.T) {
	cmd := removeCmd()
	if cmd.Use != "remove <name>" {
		t.Fatalf("unexpected use: %q", cmd.Use)
	}

	if !slices.Equal(cmd.Aliases, []string{"rm", "delete"}) {
		t.Fatalf("unexpected aliases: %#v", cmd.Aliases)
	}

	if err := cmd.Args(cmd, nil); err == nil {
		t.Fatal("expected args validation error for missing args")
	}
	if err := cmd.Args(cmd, []string{"a", "b"}); err == nil {
		t.Fatal("expected args validation error for too many args")
	}
	if err := cmd.Args(cmd, []string{"a"}); err != nil {
		t.Fatalf("expected one arg to be accepted, got %v", err)
	}
}

func TestRemoveCmdBindsContextFlag(t *testing.T) {
	cmd := removeCmd()
	if cmd.Flags().Lookup("context") == nil {
		t.Fatal("expected --context flag")
	}
}
