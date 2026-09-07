package main

import (
	"context"
	"encoding/json"
	"strings"
	"testing"
)

func TestCommandSecretRejectsDriverOptions(t *testing.T) {
	for _, options := range []any{object{"command": "printf two"}, object{}, nil} {
		_, err := secretSource("token", object{"x-command": "printf one", "driver_opts": options})
		if err == nil || !strings.Contains(err.Error(), "x-command cannot be combined with driver or driver_opts") {
			t.Fatalf("accepted conflicting driver_opts or lost diagnostic: %v", err)
		}
	}
}

func TestProvisionedVolumeMaximumBytesIsAJsonNumber(t *testing.T) {
	result, err := run(context.Background(), request{
		Version:    1,
		YAML:       "services: {app: {image: app, volumes: [data:/data]}}\nx-volumes: {data: 10G}\n",
		WorkingDir: t.TempDir(),
	})
	if err != nil {
		t.Fatal(err)
	}
	encoded, err := json.Marshal(result)
	if err != nil {
		t.Fatal(err)
	}
	var payload struct {
		Services map[string]struct {
			Volumes []struct {
				Source struct {
					MaximumBytes json.RawMessage `json:"maximum_bytes"`
				} `json:"source"`
			} `json:"volumes"`
		} `json:"services"`
	}
	if err := json.Unmarshal(encoded, &payload); err != nil {
		t.Fatal(err)
	}
	raw := payload.Services["app"].Volumes[0].Source.MaximumBytes
	if len(raw) == 0 || raw[0] == '"' {
		t.Fatalf("maximum_bytes must be a JSON number, got %s", raw)
	}
	var bytes uint64
	if err := json.Unmarshal(raw, &bytes); err != nil {
		t.Fatalf("maximum_bytes %s: %v", raw, err)
	}
	if bytes != 10<<30 {
		t.Fatalf("got %d", bytes)
	}
}
