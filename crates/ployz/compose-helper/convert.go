package main

import (
	"context"
	"fmt"
	"gopkg.in/yaml.v3"
	"maps"
	"math"
	"slices"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/compose-spec/compose-go/v2/types"
)

func convert(p *types.Project, model, provisioned, secrets object) (object, error) {
	images, err := inspectImages(context.Background(), p.WorkingDir)
	if err != nil {
		return nil, err
	}
	services, builds, dependencies, profiles := object{}, object{}, object{}, object{}
	warnings := []string{}
	for _, name := range p.ServiceNames() {
		s := p.Services[name]
		raw := mapping(mapping(model["services"])[name])
		spec, err := service(p, s, raw, provisioned)
		if err != nil {
			return nil, fmt.Errorf("service '%s': %w", name, err)
		}
		image, err := images.image(p.Name, name, s.Image, s.Build != nil)
		if err != nil {
			return nil, err
		}
		mapping(spec["container"])["image"] = image
		services[name] = spec
		if s.Build != nil {
			// Compose's JSON marshaler drops nested extensions such as x-bake.
			data, err := yaml.Marshal(s.Build)
			if err != nil {
				return nil, err
			}
			var build object
			if err := yaml.Unmarshal(data, &build); err != nil {
				return nil, err
			}
			builds[name] = object{"raw": build}
		}
		deps := []any{}
		for _, dep := range slices.Sorted(maps.Keys(s.DependsOn)) {
			d := s.DependsOn[dep]
			source := mapping(mapping(raw["depends_on"])[dep])
			if source["restart"] != nil {
				return nil, fmt.Errorf("service '%s': depends_on service '%s' uses unsupported 'restart'", name, dep)
			}
			if source["required"] == false {
				return nil, fmt.Errorf("service '%s': depends_on service '%s' uses unsupported 'required: false'", name, dep)
			}
			condition := d.Condition
			if condition == "" {
				condition = "service_started"
			}
			if condition != "service_started" && condition != "service_healthy" {
				return nil, fmt.Errorf("depends_on condition '%s' is not supported; use x-pre_deploy", condition)
			}
			target, ok := p.Services[dep]
			if !ok {
				return nil, fmt.Errorf("service '%s' depends on undefined service '%s'", name, dep)
			}
			if condition == "service_healthy" && (target.HealthCheck == nil || target.HealthCheck.Disable || len(target.HealthCheck.Test) == 0 || target.HealthCheck.Test[0] == "NONE") {
				return nil, fmt.Errorf("service '%s': depends_on service '%s' uses condition 'service_healthy', but that service has no configured healthcheck", name, dep)
			}
			deps = append(deps, object{"service": dep, "condition": condition})
		}
		dependencies[name] = deps
		if len(s.Profiles) > 0 {
			profiles[name] = s.Profiles
		}
		w, err := classify(name, raw, s.Extensions)
		if err != nil {
			return nil, err
		}
		warnings = append(warnings, w...)
	}
	return object{"name": p.Name, "working_dir": p.WorkingDir, "context": p.Extensions["x-context"], "services": services, "builds": builds, "dependencies": dependencies, "service_profiles": profiles, "warnings": warnings, "secrets": secrets, "environment": p.Environment}, nil
}

func service(p *types.Project, s types.ServiceConfig, raw, provisioned object) (object, error) {
	env := map[string]string{}
	for k, v := range s.Environment {
		if v != nil {
			env[k] = *v
		}
	}
	container := object{"image": s.Image, "environment": env, "privileged": s.Privileged, "tty": s.Tty, "open_stdin": s.StdinOpen, "pull_policy": "missing", "log_driver": object{"name": "local"}}
	if s.Command != nil {
		container["command"] = s.Command
	}
	if s.Entrypoint != nil {
		container["entrypoint"] = s.Entrypoint
	}
	if s.Labels != nil {
		container["labels"] = s.Labels
	}
	if s.Sysctls != nil {
		container["sysctls"] = s.Sysctls
	}
	if s.CapAdd != nil {
		container["cap_add"] = s.CapAdd
	}
	if s.CapDrop != nil {
		container["cap_drop"] = s.CapDrop
	}
	if s.Init != nil {
		container["init"] = s.Init
	}
	for k, v := range map[string]string{"hostname": s.Hostname, "user": s.User, "working_directory": s.WorkingDir, "pid_mode": s.Pid} {
		if v != "" {
			container[k] = v
		}
	}
	if s.PullPolicy != "" && s.PullPolicy != "if_not_present" {
		if s.PullPolicy != "missing" && s.PullPolicy != "always" && s.PullPolicy != "never" {
			return nil, fmt.Errorf("unsupported pull policy: '%s'", s.PullPolicy)
		}
		container["pull_policy"] = s.PullPolicy
	}
	if s.Logging != nil && s.Logging.Driver != "" {
		log := object{"name": s.Logging.Driver}
		if s.Logging.Options != nil {
			log["options"] = s.Logging.Options
		}
		container["log_driver"] = log
	}
	if s.Restart != "" {
		parts := strings.Split(s.Restart, ":")
		policy := parts[0]
		if policy != "no" && policy != "always" && policy != "unless-stopped" && policy != "on-failure" || len(parts) > 2 || len(parts) == 2 && policy != "on-failure" {
			return nil, fmt.Errorf("invalid restart policy %q", s.Restart)
		}
		restart := object{"name": policy}
		if len(parts) == 2 {
			count, err := strconv.ParseUint(parts[1], 10, 32)
			if err != nil {
				return nil, fmt.Errorf("invalid restart policy: %w", err)
			}
			restart["maximum_retry_count"] = count
		}
		container["restart"] = restart
	}
	if s.StopGracePeriod != nil {
		container["stop_timeout_secs"] = time.Duration(*s.StopGracePeriod).Milliseconds() / 1000
	}
	if s.HealthCheck != nil {
		h := s.HealthCheck
		health := object{"state": "disabled"}
		if !h.Disable && !(len(h.Test) > 0 && h.Test[0] == "NONE") {
			health = object{"state": "configured", "test": append([]string{}, h.Test...)}
			for key, d := range map[string]*types.Duration{"interval_millis": h.Interval, "timeout_millis": h.Timeout, "start_period_millis": h.StartPeriod, "start_interval_millis": h.StartInterval} {
				if d != nil {
					health[key] = time.Duration(*d).Milliseconds()
				}
			}
			if h.Retries != nil {
				health["retries"] = h.Retries
			}
		}
		container["healthcheck"] = health
	}
	hosts := []string{}
	for _, name := range slices.Sorted(maps.Keys(s.ExtraHosts)) {
		for _, address := range s.ExtraHosts[name] {
			hosts = append(hosts, name+":"+address)
		}
	}
	container["extra_hosts"] = hosts
	resources, err := resources(s, raw)
	if err != nil {
		return nil, err
	}
	container["resources"] = resources
	replicas := 1
	if s.Scale != nil {
		replicas = *s.Scale
	}
	mode := object{"mode": "replicated"}
	update := object{}
	if s.Deploy != nil {
		if s.Deploy.Replicas != nil {
			replicas = *s.Deploy.Replicas
		}
		if s.Deploy.Mode != "" {
			mode["mode"] = s.Deploy.Mode
		}
		if s.Deploy.UpdateConfig != nil {
			u := s.Deploy.UpdateConfig
			if u.Order != "" {
				update["order"] = strings.ReplaceAll(u.Order, "-", "_")
			}
			if mapping(mapping(raw["deploy"])["update_config"])["monitor"] != nil {
				update["monitor_millis"] = time.Duration(u.Monitor).Milliseconds()
			}
		}
	}
	if mode["mode"] == "replicated" {
		mode["replicas"] = replicas
	}
	spec := object{"name": s.Name, "mode": mode, "container": container, "update": update}
	if err := extensions(p, s, provisioned, spec); err != nil {
		return nil, err
	}
	return spec, nil
}

func resources(s types.ServiceConfig, raw object) (object, error) {
	out := object{}
	if value, ok := raw["cpus"]; ok {
		nanos, err := cpuNanos(value)
		if err != nil {
			return nil, err
		}
		out["cpu_nanos"] = nanos
	}
	for field, entry := range map[string]struct {
		key   string
		value types.UnitBytes
	}{"mem_limit": {"memory_bytes", s.MemLimit}, "mem_reservation": {"memory_reservation_bytes", s.MemReservation}, "shm_size": {"shared_memory_bytes", s.ShmSize}} {
		if raw[field] != nil {
			out[entry.key] = int64(entry.value)
		}
	}
	devices, reservations := []any{}, []any{}
	cdi := []string{}
	for _, d := range s.Devices {
		if d.Source == d.Target && strings.Contains(d.Source, "/") && strings.Contains(d.Source, "=") {
			cdi = append(cdi, d.Source)
			continue
		}
		devices = append(devices, object{"machine_path": d.Source, "container_path": d.Target, "cgroup_permissions": d.Permissions})
	}
	if len(cdi) > 0 {
		reservations = append(reservations, object{"driver": "cdi", "device_ids": cdi})
	}
	for _, d := range s.Gpus {
		reservations = append(reservations, deviceRequest(d, true))
	}
	if s.Deploy != nil {
		if r := s.Deploy.Resources.Limits; r != nil {
			limits := mapping(mapping(mapping(raw["deploy"])["resources"])["limits"])
			if value, ok := limits["cpus"]; ok {
				nanos, err := cpuNanos(value)
				if err != nil {
					return nil, err
				}
				out["cpu_nanos"] = nanos
			}
			if r.MemoryBytes != 0 {
				out["memory_bytes"] = int64(r.MemoryBytes)
			}
		}
		if r := s.Deploy.Resources.Reservations; r != nil {
			if r.MemoryBytes != 0 {
				out["memory_reservation_bytes"] = int64(r.MemoryBytes)
			}
			for _, d := range r.Devices {
				reservations = append(reservations, deviceRequest(d, false))
			}
		}
	}
	out["devices"], out["device_reservations"] = devices, reservations
	ulimits := object{}
	for name, u := range s.Ulimits {
		soft, hard := u.Soft, u.Hard
		if u.Single != 0 {
			soft, hard = u.Single, u.Single
		}
		ulimits[name] = object{"soft": soft, "hard": hard}
	}
	out["ulimits"] = ulimits
	return out, nil
}
func deviceRequest(d types.DeviceRequest, gpu bool) object {
	capabilities := append([]string{}, d.Capabilities...)
	if gpu && !slices.Contains(capabilities, "gpu") {
		capabilities = append(capabilities, "gpu")
	}
	out := object{"count": int64(d.Count), "device_ids": append([]string{}, d.IDs...), "capabilities": [][]string{capabilities}}
	if d.Driver != "" {
		out["driver"] = d.Driver
	}
	if d.Options != nil {
		out["options"] = d.Options
	}
	return out
}
func classify(name string, raw, extensions object) ([]string, error) {
	warnings := []string{}
	if _, ok := extensions["x-volumes"]; ok {
		return nil, fmt.Errorf("service '%s': x-volumes is only supported at Compose top level", name)
	}
	if raw["read_only"] == true {
		return nil, fmt.Errorf("service '%s': unsupported feature 'read_only'", name)
	}
	if raw["security_opt"] != nil {
		return nil, fmt.Errorf("service '%s': unsupported feature 'security_opt'", name)
	}
	if mapping(raw["deploy"])["placement"] != nil {
		return nil, fmt.Errorf("service '%s': unsupported feature 'deploy.placement'; use x-machines", name)
	}
	for _, key := range []string{"dns", "dns_opt", "dns_search", "group_add", "ipc", "links", "network_mode", "oom_kill_disable", "pids_limit", "runtime", "storage_opt", "tmpfs", "userns_mode", "uts", "volumes_from"} {
		if raw[key] != nil {
			warnings = append(warnings, fmt.Sprintf("service '%s': unsupported feature '%s'", name, key))
		}
	}
	for _, pair := range [][2]string{{"x-port", "x-ports"}, {"x-machine", "x-machines"}} {
		if _, ok := extensions[pair[0]]; ok {
			warnings = append(warnings, fmt.Sprintf("service '%s': unsupported feature '%s'; use %s", name, pair[0], pair[1]))
		}
	}
	for _, key := range []string{"mem_swappiness", "memswap_limit"} {
		if n, _ := strconv.ParseFloat(fmt.Sprint(raw[key]), 64); n > 0 {
			warnings = append(warnings, fmt.Sprintf("service '%s': unsupported feature '%s'", name, key))
		}
	}
	for key := range mapping(raw["networks"]) {
		if key != "default" {
			warnings = append(warnings, fmt.Sprintf("service '%s': unsupported feature 'networks'", name))
			break
		}
	}
	if v, ok := raw["secrets"].([]any); ok && len(v) > 0 {
		warnings = append(warnings, fmt.Sprintf("service '%s': unsupported feature 'secrets'", name))
	}
	deployKeys := []string{}
	for key, value := range mapping(raw["deploy"]) {
		if value != nil && key != "mode" && key != "replicas" && key != "resources" && key != "update_config" && !strings.HasPrefix(key, "x-") && key != "#extensions" {
			deployKeys = append(deployKeys, key)
		}
	}
	sort.Strings(deployKeys)
	for _, key := range deployKeys {
		warnings = append(warnings, fmt.Sprintf("service '%s': unsupported feature 'deploy.%s'", name, key))
	}
	return warnings, nil
}

func cpuNanos(value any) (int64, error) {
	cpus, err := strconv.ParseFloat(fmt.Sprint(value), 64)
	if err != nil {
		return 0, fmt.Errorf("cpus must be numeric")
	}
	nanos := cpus * 1e9
	if math.IsNaN(nanos) || math.IsInf(nanos, 0) || nanos < 0 || nanos >= float64(math.MaxInt64) {
		return 0, fmt.Errorf("invalid cpu quantity")
	}
	return int64(nanos), nil
}
