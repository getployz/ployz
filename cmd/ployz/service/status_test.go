package service

import "testing"

func TestStatusCmdShape(t *testing.T) {
	cmd := statusCmd()
	if cmd.Use != "status <service>" {
		t.Fatalf("unexpected use: %q", cmd.Use)
	}
	if err := cmd.Args(cmd, []string{}); err == nil {
		t.Fatal("expected args validation error for missing service")
	}
	if err := cmd.Args(cmd, []string{"a", "b"}); err == nil {
		t.Fatal("expected args validation error for too many args")
	}
}

func TestStatusCmdBindsContextFlag(t *testing.T) {
	cmd := statusCmd()
	if cmd.Flags().Lookup("context") == nil {
		t.Fatal("expected --context flag")
	}
}
