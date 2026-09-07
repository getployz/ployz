package main

import (
	"crypto/sha256"
	"fmt"
	"maps"
	"math"
	"os"
	"path/filepath"
	"reflect"
	"slices"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/compose-spec/compose-go/v2/loader"
	"github.com/compose-spec/compose-go/v2/types"
)

func extensions(p *types.Project, s types.ServiceConfig, provisioned, spec object) error {
	machines, err := stringList(s.Extensions["x-machines"])
	if err != nil {
		return err
	}
	spec["placement"] = object{"machines": machines}
	publications := []any{}
	if x, ok := s.Extensions["x-ports"]; ok {
		if len(s.Ports) > 0 {
			return fmt.Errorf("cannot specify both 'ports' and 'x-ports'")
		}
		ports, err := stringList(x)
		if err != nil {
			return err
		}
		for _, value := range ports {
			port, err := extensionPort(value)
			if err != nil {
				return err
			}
			publications = append(publications, port)
		}
	} else {
		for _, v := range s.Ports {
			if strings.Contains(v.Published, "-") {
				return fmt.Errorf("published port ranges are not supported")
			}
			if v.Mode != "host" {
				return fmt.Errorf("TCP/UDP ingress is not supported; use host publication (for example 8080:80/tcp@host or mode: host)")
			}
			port, err := hostPort(v.HostIP, v.Published, int(v.Target), v.Protocol)
			if err != nil {
				return err
			}
			publications = append(publications, port)
		}
	}
	spec["ports"] = publications
	if value, ok := s.Extensions["x-caddy"]; ok {
		text, ok := value.(string)
		if !ok {
			m := mapping(value)
			if m == nil {
				return fmt.Errorf("x-caddy must be a string or map")
			}
			for key := range m {
				if key != "config" {
					return fmt.Errorf("invalid x-caddy key: %s", key)
				}
			}
			text, _ = m["config"].(string)
		}
		if text != "" && !strings.Contains(text, "\n") {
			data, err := os.ReadFile(localPath(p.WorkingDir, text))
			if err != nil {
				return err
			}
			text = string(data)
		}
		if text = strings.TrimSpace(text); text != "" {
			for _, port := range publications {
				if mapping(port)["mode"] == "ingress" {
					return fmt.Errorf("ingress ports and 'x-caddy' cannot be specified simultaneously")
				}
			}
			spec["ingress_proxy_fragment"] = text
		}
	}
	if value, ok := s.Extensions["x-pre_deploy"]; ok {
		hook := mapping(value)
		if hook == nil {
			return fmt.Errorf("x-pre_deploy must be a map")
		}
		for key := range hook {
			if key != "command" && key != "environment" && key != "privileged" && key != "timeout" {
				return fmt.Errorf("invalid x-pre_deploy key: %s", key)
			}
		}
		var translated struct {
			Command     types.ShellCommand
			Environment types.MappingWithEquals
			Privileged  *bool
			Timeout     *types.Duration
		}
		if err := loader.Transform(hook, &translated); err != nil {
			return err
		}
		env := object{}
		for k, v := range translated.Environment {
			if v != nil {
				env[k] = *v
			}
		}
		pre := object{"command": append([]string{}, translated.Command...), "environment": env, "privileged": translated.Privileged}
		if translated.Timeout != nil {
			pre["timeout_millis"] = time.Duration(*translated.Timeout).Milliseconds()
		}
		spec["pre_deploy"] = pre
	}
	return storage(p, s, provisioned, spec)
}
func stringList(value any) ([]string, error) {
	if value == nil {
		return []string{}, nil
	}
	list := []string{}
	switch v := value.(type) {
	case string:
		list = strings.Split(v, ",")
	case []any:
		for _, item := range v {
			text, ok := item.(string)
			if !ok {
				return nil, fmt.Errorf("value must be a string")
			}
			list = append(list, text)
		}
	case []string:
		list = v
	default:
		return nil, fmt.Errorf("value must be a string or list")
	}
	for i, v := range list {
		list[i] = strings.TrimSpace(v)
		if list[i] == "" {
			return nil, fmt.Errorf("value cannot be empty")
		}
	}
	return list, nil
}
func extensionPort(value string) (object, error) {
	host := strings.HasSuffix(value, "@host")
	if strings.Count(value, "@") > 1 {
		return nil, fmt.Errorf("too many '@' symbols in port")
	}
	if strings.Contains(value, "@") && !host {
		return nil, fmt.Errorf("invalid port mode in '%s'", value)
	}
	value = strings.TrimSuffix(value, "@host")
	protocol := "tcp"
	if i := strings.LastIndex(value, "/"); i >= 0 {
		protocol = value[i+1:]
		value = value[:i]
	}
	parts := strings.Split(value, ":")
	if len(parts) > 3 {
		parts = []string{strings.Join(parts[:len(parts)-2], ":"), parts[len(parts)-2], parts[len(parts)-1]}
	}
	target, err := portNumber(parts[len(parts)-1])
	if err != nil {
		return nil, err
	}
	if host {
		switch len(parts) {
		case 2:
			return hostPort("", parts[0], target, protocol)
		case 3:
			return hostPort(parts[0], parts[1], target, protocol)
		default:
			return nil, fmt.Errorf("invalid host port '%s'", value)
		}
	}
	if protocol != "http" && protocol != "https" {
		return nil, fmt.Errorf("TCP/UDP ingress is not supported; use host publication")
	}
	hostname, published := "", ""
	switch len(parts) {
	case 1:
	case 2:
		if parts[0] != "" && strings.Trim(parts[0], "0123456789") == "" {
			published = parts[0]
		} else {
			hostname = parts[0]
		}
	case 3:
		hostname, published = parts[0], parts[1]
	default:
		return nil, fmt.Errorf("invalid ingress port '%s'", value)
	}
	port := 80
	if protocol == "https" {
		port = 443
	}
	if published != "" {
		port, err = portNumber(published)
		if err != nil {
			return nil, fmt.Errorf("published %w", err)
		}
	}
	h := object{"kind": "cluster_domain"}
	if strings.Contains(hostname, ".") {
		h = object{"kind": "explicit", "hostname": hostname}
	} else if hostname != "" {
		h["label"] = hostname
	}
	return object{"mode": "ingress", "hostname": h, "load_balancer_port": port, "container_port": target, "http_protocol": protocol}, nil
}
func portNumber(value string) (int, error) {
	if strings.Contains(value, "-") {
		return 0, fmt.Errorf("published port ranges are not supported")
	}
	n, err := strconv.ParseUint(value, 10, 16)
	if err != nil {
		return 0, fmt.Errorf("invalid port '%s'", value)
	}
	if n == 0 {
		return 0, fmt.Errorf("port must be non-zero")
	}
	return int(n), nil
}
func hostPort(host, published string, target int, protocol string) (object, error) {
	if protocol == "" {
		protocol = "tcp"
	}
	if protocol != "tcp" && protocol != "udp" {
		return nil, fmt.Errorf("unsupported protocol '%s' in host mode", protocol)
	}
	if published == "" {
		return nil, fmt.Errorf("published port is required in host mode")
	}
	port, err := portNumber(published)
	if err != nil {
		return nil, err
	}
	bind := object{"kind": "all"}
	if host != "" && host != "0.0.0.0" && host != "::" {
		if strings.Contains(host, "/") {
			bind = object{"kind": "prefix", "prefix": strings.ReplaceAll(strings.ReplaceAll(host, "[", ""), "]", "")}
		} else {
			bind = object{"kind": "address", "address": strings.Trim(host, "[]")}
		}
	}
	return object{"mode": "host", "bind": bind, "published_port": port, "container_port": target, "transport_protocol": protocol}, nil
}
func storage(p *types.Project, s types.ServiceConfig, provisioned, spec object) error {
	sources := object{}
	mounts := []any{}
	for _, v := range s.Volumes {
		reference := v.Source
		source := object{}
		switch v.Type {
		case "bind":
			if !filepath.IsAbs(v.Source) {
				return fmt.Errorf("bind mount source '%s' is relative", v.Source)
			}
			reference = fmt.Sprintf("bind-%x", sha256.Sum256([]byte(v.Target)))
			source = object{"kind": "bind", "machine_path": v.Source}
			if v.Bind != nil {
				source["create_machine_path"] = v.Bind.CreateHostPath
				if v.Bind.Propagation != "" {
					if !strings.Contains("|private|rprivate|shared|rshared|slave|rslave|", "|"+v.Bind.Propagation+"|") {
						return fmt.Errorf("invalid bind propagation %q", v.Bind.Propagation)
					}
					source["propagation"] = v.Bind.Propagation
				}
				if v.Bind.Recursive != "" {
					if v.Bind.Recursive != "disabled" && v.Bind.Recursive != "writable" && v.Bind.Recursive != "readonly" {
						return fmt.Errorf("invalid bind recursive %q", v.Bind.Recursive)
					}
					source["recursive"] = v.Bind.Recursive
				}
			}
		case "tmpfs":
			reference = fmt.Sprintf("tmpfs-%x", sha256.Sum256([]byte(v.Target)))
			source = object{"kind": "tmpfs"}
			if v.Tmpfs != nil {
				if v.Tmpfs.Size != 0 {
					source["size_bytes"] = uint64(v.Tmpfs.Size)
				}
				if v.Tmpfs.Mode != 0 {
					source["mode"] = v.Tmpfs.Mode
				}
			}
		case "volume":
			declared, ok := p.Volumes[v.Source]
			if !ok {
				return fmt.Errorf("volume '%s' not found in project volumes", v.Source)
			}
			if value, ok := provisioned[v.Source]; ok {
				size := value.(uint64)
				source = object{"kind": "provisioned", "name": v.Source, "maximum_bytes": size}
			} else if declared.External {
				name := declared.Name
				if name == "" {
					name = v.Source
				}
				source = object{"kind": "external", "name": name}
			} else {
				driver := declared.Driver
				if driver == "" && len(declared.DriverOpts) > 0 {
					return fmt.Errorf("volume '%s': driver_opts requires driver", v.Source)
				}
				if driver == "" {
					driver = "local"
				}
				options := object{}
				for k, v := range declared.DriverOpts {
					options[k] = v
				}
				labels := object{}
				for k, v := range declared.Labels {
					labels[k] = v
				}
				source = object{"kind": "ordinary", "name": v.Source, "driver": object{"name": driver, "options": options}, "labels": labels}
			}
		default:
			return fmt.Errorf("unsupported volume type: '%s'", v.Type)
		}
		if prior, ok := sources[reference]; ok && !reflect.DeepEqual(prior, source) {
			return fmt.Errorf("volume '%s' is used multiple times with different options", reference)
		}
		sources[reference] = source
		mount := object{"volume": reference, "target": v.Target, "read_only": v.ReadOnly}
		if v.Volume != nil {
			mount["no_copy"] = v.Volume.NoCopy
			if v.Volume.Subpath != "" {
				mount["subpath"] = v.Volume.Subpath
			}
		}
		mounts = append(mounts, mount)
	}
	volumes := []any{}
	for _, name := range slices.Sorted(maps.Keys(sources)) {
		volumes = append(volumes, object{"reference": name, "source": sources[name]})
	}
	spec["volumes"], spec["mounts"] = volumes, mounts
	configs, configMounts := []any{}, []any{}
	seen := map[string]bool{}
	for _, v := range s.Configs {
		definition, ok := p.Configs[v.Source]
		if !ok {
			return fmt.Errorf("config '%s' not found in project configs", v.Source)
		}
		if definition.External {
			return fmt.Errorf("external configs are not supported: %s", v.Source)
		}
		content := []byte(definition.Content)
		if definition.File != "" {
			var err error
			content, err = os.ReadFile(localPath(p.WorkingDir, definition.File))
			if err != nil {
				return err
			}
		}
		if !seen[v.Source] {
			bytes := make([]uint32, len(content))
			for i, b := range content {
				bytes[i] = uint32(b)
			}
			configs = append(configs, object{"name": v.Source, "content": bytes})
			seen[v.Source] = true
		}
		target := v.Target
		if target == "" {
			target = "/" + v.Source
		}
		mount := object{"config_name": v.Source, "target": target}
		if v.Mode != nil {
			mount["mode"] = int64(*v.Mode)
		}
		for key, value := range map[string]string{"uid": v.UID, "gid": v.GID} {
			if value != "" {
				n, err := strconv.ParseUint(value, 10, 64)
				if err != nil {
					return err
				}
				mount[key] = n
			}
		}
		configMounts = append(configMounts, mount)
	}
	sort.Slice(configs, func(i, j int) bool {
		return mapping(configs[i])["name"].(string) < mapping(configs[j])["name"].(string)
	})
	spec["configs"] = configs
	mapping(spec["container"])["config_mounts"] = configMounts
	return nil
}
func provisionedSize(value any) (uint64, error) {
	text, ok := value.(string)
	if !ok {
		m := mapping(value)
		if len(m) != 1 {
			return 0, fmt.Errorf("expected a size or {size: value}")
		}
		text, ok = m["size"].(string)
	}
	if !ok || len(text) < 2 {
		return 0, fmt.Errorf("invalid Volume size")
	}
	text = strings.ToLower(text)
	power := strings.IndexByte("kmgt", text[len(text)-1]) + 1
	if power == 0 {
		return 0, fmt.Errorf("invalid Volume size %q", text)
	}
	amount, err := strconv.ParseUint(text[:len(text)-1], 10, 64)
	multiplier := uint64(1) << (10 * power)
	if err != nil || amount == 0 || amount > math.MaxUint64/multiplier {
		return 0, fmt.Errorf("invalid or overflowing Volume size %q", text)
	}
	return amount * multiplier, nil
}

func localPath(directory, path string) string {
	if filepath.IsAbs(path) {
		return path
	}
	return filepath.Join(directory, path)
}
