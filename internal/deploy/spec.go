package deploy

import (
	"reflect"
	"slices"
	"sort"
	"strconv"
	"strings"
	"time"

	compose "github.com/compose-spec/compose-go/v2/types"
)

// NormalizeServiceSpec extracts the fields we care about from a compose
// ServiceConfig into our JSON-serializable ServiceSpec.
func NormalizeServiceSpec(svc compose.ServiceConfig) ServiceSpec {
	spec := ServiceSpec{
		Name:          svc.Name,
		Image:         svc.Image,
		Command:       normalizeStringSlice([]string(svc.Command), false),
		Entrypoint:    normalizeStringSlice([]string(svc.Entrypoint), false),
		Environment:   normalizeEnvironment(svc.Environment),
		Mounts:        normalizeMounts(svc.Volumes),
		Ports:         normalizePorts(svc.Ports),
		Labels:        normalizeLabels(svc.Labels),
		RestartPolicy: normalizeRestartPolicy(svc),
		HealthCheck:   normalizeHealthCheck(svc.HealthCheck),
		Resources:     normalizeDeployResources(svc.Deploy),
	}
	return canonicalSpec(spec)
}

// ClassifyChange compares two ServiceSpecs and returns what kind of change
// is needed.
func ClassifyChange(current, incoming ServiceSpec) ChangeKind {
	if SpecEqual(current, incoming) {
		return UpToDate
	}

	currentWithoutResources := current
	currentWithoutResources.Resources = nil
	incomingWithoutResources := incoming
	incomingWithoutResources.Resources = nil
	if SpecEqual(currentWithoutResources, incomingWithoutResources) {
		return NeedsUpdate
	}

	return NeedsRecreate
}

// SpecEqual returns true if two ServiceSpecs are functionally identical.
func SpecEqual(a, b ServiceSpec) bool {
	a = canonicalSpec(a)
	b = canonicalSpec(b)
	return reflect.DeepEqual(a, b)
}

func canonicalSpec(spec ServiceSpec) ServiceSpec {
	out := ServiceSpec{
		Name:          spec.Name,
		Image:         spec.Image,
		Command:       normalizeStringSlice(spec.Command, false),
		Entrypoint:    normalizeStringSlice(spec.Entrypoint, false),
		Environment:   normalizeStringSlice(spec.Environment, true),
		Mounts:        normalizeMountEntries(spec.Mounts),
		Ports:         normalizePortEntries(spec.Ports),
		Labels:        normalizeLabelMap(spec.Labels),
		RestartPolicy: spec.RestartPolicy,
		HealthCheck:   normalizeHealthCheckEntry(spec.HealthCheck),
		Resources:     normalizeResourcesEntry(spec.Resources),
	}
	return out
}

func normalizeEnvironment(env compose.MappingWithEquals) []string {
	if len(env) == 0 {
		return nil
	}

	keys := make([]string, 0, len(env))
	for k := range env {
		keys = append(keys, k)
	}
	sort.Strings(keys)

	out := make([]string, 0, len(keys))
	for _, key := range keys {
		value := ""
		if p := env[key]; p != nil {
			value = *p
		}
		out = append(out, key+"="+value)
	}
	return out
}

func normalizeMounts(volumes []compose.ServiceVolumeConfig) []Mount {
	if len(volumes) == 0 {
		return nil
	}

	out := make([]Mount, 0, len(volumes))
	for _, v := range volumes {
		if strings.TrimSpace(v.Target) == "" {
			continue
		}
		out = append(out, Mount{
			Source:   v.Source,
			Target:   v.Target,
			ReadOnly: v.ReadOnly,
		})
	}

	return normalizeMountEntries(out)
}

func normalizePorts(ports []compose.ServicePortConfig) []PortMapping {
	if len(ports) == 0 {
		return nil
	}

	out := make([]PortMapping, 0, len(ports))
	for _, p := range ports {
		protocol := strings.ToLower(strings.TrimSpace(p.Protocol))
		if protocol == "" {
			protocol = "tcp"
		}

		containerPort := uint16(0)
		if p.Target <= uint32(^uint16(0)) {
			containerPort = uint16(p.Target)
		}

		out = append(out, PortMapping{
			HostPort:      parsePublishedPort(p.Published),
			ContainerPort: containerPort,
			Protocol:      protocol,
		})
	}

	return normalizePortEntries(out)
}

func parsePublishedPort(published string) uint16 {
	published = strings.TrimSpace(published)
	if published == "" {
		return 0
	}
	n, err := strconv.ParseUint(published, 10, 16)
	if err != nil {
		return 0
	}
	return uint16(n)
}

func normalizeLabels(labels compose.Labels) map[string]string {
	if len(labels) == 0 {
		return nil
	}
	out := make(map[string]string, len(labels))
	for key, value := range labels {
		out[key] = value
	}
	return out
}

func normalizeRestartPolicy(svc compose.ServiceConfig) string {
	if restart := strings.TrimSpace(svc.Restart); restart != "" {
		return restart
	}
	if svc.Deploy != nil && svc.Deploy.RestartPolicy != nil {
		return strings.TrimSpace(svc.Deploy.RestartPolicy.Condition)
	}
	return ""
}

func normalizeHealthCheck(hc *compose.HealthCheckConfig) *HealthCheck {
	if hc == nil || hc.Disable {
		return nil
	}

	out := &HealthCheck{
		Test:        normalizeStringSlice([]string(hc.Test), false),
		Interval:    composeDuration(hc.Interval),
		Timeout:     composeDuration(hc.Timeout),
		Retries:     retriesValue(hc.Retries),
		StartPeriod: composeDuration(hc.StartPeriod),
	}
	return out
}

func composeDuration(d *compose.Duration) time.Duration {
	if d == nil {
		return 0
	}
	return time.Duration(*d)
}

func retriesValue(retries *uint64) int {
	if retries == nil {
		return 0
	}
	const maxInt = int(^uint(0) >> 1)
	if *retries > uint64(maxInt) {
		return maxInt
	}
	return int(*retries)
}

func normalizeDeployResources(cfg *compose.DeployConfig) *Resources {
	if cfg == nil || cfg.Resources.Limits == nil {
		return nil
	}
	limits := cfg.Resources.Limits
	out := &Resources{
		CPULimit:    float64(limits.NanoCPUs.Value()),
		MemoryLimit: int64(limits.MemoryBytes),
	}
	if out.CPULimit == 0 && out.MemoryLimit == 0 {
		return nil
	}
	return out
}

func normalizeStringSlice(values []string, sortValues bool) []string {
	if len(values) == 0 {
		return nil
	}
	out := make([]string, len(values))
	copy(out, values)
	if sortValues {
		slices.Sort(out)
	}
	return out
}

func normalizeMountEntries(entries []Mount) []Mount {
	if len(entries) == 0 {
		return nil
	}
	out := make([]Mount, len(entries))
	copy(out, entries)
	sort.Slice(out, func(i, j int) bool {
		if out[i].Source != out[j].Source {
			return out[i].Source < out[j].Source
		}
		if out[i].Target != out[j].Target {
			return out[i].Target < out[j].Target
		}
		return !out[i].ReadOnly && out[j].ReadOnly
	})
	return out
}

func normalizePortEntries(entries []PortMapping) []PortMapping {
	if len(entries) == 0 {
		return nil
	}
	out := make([]PortMapping, len(entries))
	copy(out, entries)
	sort.Slice(out, func(i, j int) bool {
		if out[i].HostPort != out[j].HostPort {
			return out[i].HostPort < out[j].HostPort
		}
		if out[i].ContainerPort != out[j].ContainerPort {
			return out[i].ContainerPort < out[j].ContainerPort
		}
		return out[i].Protocol < out[j].Protocol
	})
	return out
}

func normalizeLabelMap(labels map[string]string) map[string]string {
	if len(labels) == 0 {
		return nil
	}
	out := make(map[string]string, len(labels))
	for key, value := range labels {
		out[key] = value
	}
	return out
}

func normalizeHealthCheckEntry(hc *HealthCheck) *HealthCheck {
	if hc == nil {
		return nil
	}
	out := &HealthCheck{
		Test:        normalizeStringSlice(hc.Test, false),
		Interval:    hc.Interval,
		Timeout:     hc.Timeout,
		Retries:     hc.Retries,
		StartPeriod: hc.StartPeriod,
	}
	return out
}

func normalizeResourcesEntry(r *Resources) *Resources {
	if r == nil {
		return nil
	}
	out := &Resources{CPULimit: r.CPULimit, MemoryLimit: r.MemoryLimit}
	if out.CPULimit == 0 && out.MemoryLimit == 0 {
		return nil
	}
	return out
}
