package network

import (
	"errors"
	"strings"
	"testing"

	"ployz/pkg/sdk/client"
)

func TestDestroyCmdShape(t *testing.T) {
	cmd := destroyCmd()
	if cmd.Use != "destroy [network]" {
		t.Fatalf("unexpected use: %q", cmd.Use)
	}

	if err := cmd.Args(cmd, []string{"a", "b"}); err == nil {
		t.Fatal("expected args validation error for too many args")
	}
}

func TestDecorateDestroyErrorActiveEndpointsFallsBackToGeneric(t *testing.T) {
	err := decorateDestroyError("default", errors.New("remove network \"ployz_default\": network has active endpoints"))
	if err == nil {
		t.Fatal("expected error")
	}
	if strings.Contains(err.Error(), "ployz service remove <name>") {
		t.Fatalf("expected generic fallback, got %q", err.Error())
	}
}

func TestDecorateDestroyErrorManagedWorkloadsHint(t *testing.T) {
	err := decorateDestroyError("default", errors.Join(client.ErrPrecondition, client.ErrNetworkDestroyHasWorkloads))
	if err == nil {
		t.Fatal("expected error")
	}
	if !strings.Contains(err.Error(), "ployz service remove <name>") {
		t.Fatalf("expected remediation hint, got %q", err.Error())
	}
}

func TestDecorateDestroyErrorAttachedMachinesHint(t *testing.T) {
	err := decorateDestroyError("default", errors.Join(client.ErrPrecondition, client.ErrNetworkDestroyHasMachines))
	if err == nil {
		t.Fatal("expected error")
	}
	if !strings.Contains(err.Error(), "ployz machine remove <id>") {
		t.Fatalf("expected remediation hint, got %q", err.Error())
	}
}
