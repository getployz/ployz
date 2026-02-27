package cluster

import "testing"

func TestCurrentPrefersContextEnv(t *testing.T) {
	t.Setenv(envContext, "ctx-a")

	cfg := &Config{Clusters: map[string]Cluster{
		"ctx-a": {Network: "a"},
		"ctx-b": {Network: "b"},
	}}

	name, _, ok := cfg.Current()
	if !ok {
		t.Fatal("expected current context")
	}
	if name != "ctx-a" {
		t.Fatalf("expected ctx-a, got %q", name)
	}
}

func TestCurrentUsesCurrentContextWhenEnvUnset(t *testing.T) {
	t.Setenv(envContext, "")

	cfg := &Config{
		CurrentCluster: "ctx-b",
		Clusters: map[string]Cluster{
			"ctx-a": {Network: "a"},
			"ctx-b": {Network: "b"},
		},
	}

	name, _, ok := cfg.Current()
	if !ok {
		t.Fatal("expected current context")
	}
	if name != "ctx-b" {
		t.Fatalf("expected ctx-b, got %q", name)
	}
}
