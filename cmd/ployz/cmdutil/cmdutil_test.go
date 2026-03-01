package cmdutil

import (
	"testing"

	"ployz/platform"
)

func TestDefaultSocketPathMatchesPlatform(t *testing.T) {
	if got, want := DefaultSocketPath(), platform.DaemonSocketPath; got != want {
		t.Fatalf("default socket path mismatch: got %q, want %q", got, want)
	}
}

func TestDefaultDataRootMatchesPlatform(t *testing.T) {
	if got, want := DefaultDataRoot(), platform.DaemonDataRoot; got != want {
		t.Fatalf("default data root mismatch: got %q, want %q", got, want)
	}
}
