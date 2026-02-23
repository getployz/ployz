package deploy

import (
	"encoding/json"
	"testing"

	compose "github.com/compose-spec/compose-go/v2/types"
)

func FuzzClassifyChange(f *testing.F) {
	f.Add("nginx:1.25", "nginx:1.25", float64(1), float64(1), int64(128*1024*1024), int64(128*1024*1024), "web", "web")
	f.Add("nginx:1.25", "nginx:1.26", float64(1), float64(2), int64(128*1024*1024), int64(256*1024*1024), "web", "api")

	f.Fuzz(func(t *testing.T, imageA, imageB string, cpuA, cpuB float64, memA, memB int64, labelA, labelB string) {
		if memA < 0 {
			memA = -memA
		}
		if memB < 0 {
			memB = -memB
		}

		current := ServiceSpec{
			Name:   "svc",
			Image:  imageA,
			Labels: map[string]string{"role": labelA},
			Resources: &Resources{
				CPULimit:    cpuA,
				MemoryLimit: memA,
			},
		}
		incoming := ServiceSpec{
			Name:   "svc",
			Image:  imageB,
			Labels: map[string]string{"role": labelB},
			Resources: &Resources{
				CPULimit:    cpuB,
				MemoryLimit: memB,
			},
		}

		if got := ClassifyChange(current, current); got != UpToDate {
			t.Fatalf("ClassifyChange(a, a) = %v, want %v", got, UpToDate)
		}

		kind := ClassifyChange(current, incoming)
		if !isValidChangeKind(kind) {
			t.Fatalf("invalid ChangeKind: %v", kind)
		}

		if SpecEqual(current, incoming) && kind != UpToDate {
			t.Fatalf("SpecEqual() true but ClassifyChange() = %v, want %v", kind, UpToDate)
		}
	})
}

func FuzzNormalizeServiceSpec(f *testing.F) {
	f.Add("api", "nginx:1.25", "A", "1", "8080", "", uint32(80), float32(1), int64(64*1024*1024), true)
	f.Add("worker", "busybox:latest", "", "", "not-a-port", "udp", uint32(65535), float32(0), int64(0), false)

	f.Fuzz(func(t *testing.T, name, image, envKey, envValue, published, protocol string, target uint32, cpu float32, memory int64, readOnly bool) {
		if memory < 0 {
			memory = -memory
		}
		target = target % 65536

		svc := compose.ServiceConfig{
			Name:        name,
			Image:       image,
			Command:     compose.ShellCommand{"run"},
			Environment: compose.MappingWithEquals{},
			Volumes: []compose.ServiceVolumeConfig{
				{Source: "data", Target: "/data", ReadOnly: readOnly},
			},
			Ports: []compose.ServicePortConfig{
				{Published: published, Target: target, Protocol: protocol},
			},
			Deploy: &compose.DeployConfig{
				Resources: compose.Resources{
					Limits: &compose.Resource{
						NanoCPUs:    compose.NanoCPUs(cpu),
						MemoryBytes: compose.UnitBytes(memory),
					},
				},
			},
		}
		if envKey != "" {
			v := envValue
			svc.Environment[envKey] = &v
		}

		normalized := NormalizeServiceSpec(svc)
		normalizedAgain := NormalizeServiceSpec(svc)
		if !SpecEqual(normalized, normalizedAgain) {
			t.Fatalf("NormalizeServiceSpec not deterministic: first=%+v second=%+v", normalized, normalizedAgain)
		}

		data, err := json.Marshal(normalized)
		if err != nil {
			t.Fatalf("json.Marshal() error = %v", err)
		}
		var roundTrip ServiceSpec
		if err := json.Unmarshal(data, &roundTrip); err != nil {
			t.Fatalf("json.Unmarshal() error = %v", err)
		}
		if !SpecEqual(normalized, roundTrip) {
			t.Fatalf("round-trip mismatch: normalized=%+v roundTrip=%+v", normalized, roundTrip)
		}
	})
}

func isValidChangeKind(kind ChangeKind) bool {
	switch kind {
	case UpToDate, NeedsSpecUpdate, NeedsUpdate, NeedsRecreate, Create, Remove:
		return true
	default:
		return false
	}
}
