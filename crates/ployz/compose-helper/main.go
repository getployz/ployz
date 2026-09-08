package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/compose-spec/compose-go/v2/cli"
	"github.com/compose-spec/compose-go/v2/loader"
	"github.com/compose-spec/compose-go/v2/schema"
	"github.com/compose-spec/compose-go/v2/types"
	"github.com/compose-spec/compose-go/v2/validation"
)

type object = map[string]any

type request struct {
	BuildContext *contextRequest `json:"build_context"`
	Version      int             `json:"version"`
	Files        []string        `json:"files"`
	Profiles     []string        `json:"profiles"`
	AllProfiles  bool            `json:"all_profiles"`
	WorkingDir   string          `json:"working_dir"`
	Port         *string         `json:"port"`
	YAML         string          `json:"yaml"`
}

func main() {
	var req request
	result := object{"version": 1}
	err := json.NewDecoder(os.Stdin).Decode(&req)
	if err == nil && req.Version != 1 {
		err = fmt.Errorf("unsupported Compose helper protocol %d", req.Version)
	}
	if err == nil {
		if req.BuildContext != nil {
			result["result"], err = contextFiles(*req.BuildContext)
		} else if req.Port != nil {
			result["result"], err = extensionPort(*req.Port)
		} else {
			result["result"], err = run(context.Background(), req)
		}
	}
	if err != nil {
		result["error"] = err.Error()
		delete(result, "result")
	}
	if err := json.NewEncoder(os.Stdout).Encode(result); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run(ctx context.Context, req request) (object, error) {
	var model object
	var env types.Mapping
	var err error
	directory, err := filepath.Abs(req.WorkingDir)
	if err != nil {
		return nil, err
	}
	profiles := req.Profiles
	if req.AllProfiles || req.YAML != "" {
		profiles = []string{"*"}
	}
	if req.YAML != "" {
		env = types.NewMapping(os.Environ())
		model, err = loader.LoadModelWithContext(ctx, types.ConfigDetails{
			WorkingDir: directory, Environment: env,
			ConfigFiles: []types.ConfigFile{{Filename: filepath.Join(directory, "compose.yaml"), Content: []byte(req.YAML)}},
		}, func(o *loader.Options) {
			o.SkipValidation = true
			o.SkipInterpolation = true
			o.SkipNormalization = true
			o.SkipDefaultValues = true
			o.ResolvePaths = false
			o.SetProjectName("project", false)
		})
	} else {
		if err := os.Chdir(directory); err != nil {
			return nil, err
		}
		options, e := cli.NewProjectOptions(req.Files,
			cli.WithOsEnv, cli.WithEnvFiles(), cli.WithDotEnv,
			cli.WithConfigFileEnv, cli.WithDefaultConfigPath, cli.WithEnvFiles(), cli.WithDotEnv,
			cli.WithResolvedPaths(false), cli.WithLoadOptions(loader.WithSkipValidation))
		if e != nil {
			return nil, e
		}
		env = options.Environment
		directory, e = options.GetWorkingDir()
		if e != nil {
			return nil, e
		}
		config, e := options.ReadConfigFiles(ctx, directory, options)
		if e != nil {
			return nil, e
		}
		config.Environment = env
		model, err = loader.LoadModelWithContext(ctx, *config, func(o *loader.Options) {
			o.SkipValidation = true
			o.ResolvePaths = false
			name := env["COMPOSE_PROJECT_NAME"]
			if name != "" {
				o.SetProjectName(name, true)
			} else {
				o.SetProjectName(loader.NormalizeProjectName(filepath.Base(directory)), false)
			}
		})

	}
	if err != nil {
		return nil, err
	}
	name, _ := model["name"].(string)
	if name == "" {
		name = filepath.Base(directory)
	}
	if name == "." || name == "/" {
		name = "project"
	}
	// Ployz extensions supply resources that Compose cannot provision itself.
	// Adapt the in-memory declarations before running upstream consistency checks.
	secrets := object{}
	for key, value := range mapping(model["secrets"]) {
		raw := mapping(value)
		source, err := secretSource(key, raw)
		if err != nil {
			return nil, err
		}
		secrets[key] = object{"Unresolved": source}
		if raw["x-command"] != nil || raw["driver"] != nil {
			raw["external"] = true
			delete(raw, "driver")
			delete(raw, "driver_opts")
		}
	}
	volumes := mapping(model["volumes"])
	if volumes == nil {
		volumes = object{}
		model["volumes"] = volumes
	}
	for key, value := range mapping(model["configs"]) {
		if mapping(value)["external"] == true {
			return nil, fmt.Errorf("external configs are not supported: %s", key)
		}
	}
	if model["x-volumes"] != nil && mapping(model["x-volumes"]) == nil {
		return nil, fmt.Errorf("x-volumes must be a map")
	}
	provisioned := object{}
	for key, value := range mapping(model["x-volumes"]) {
		if _, exists := volumes[key]; exists {
			return nil, fmt.Errorf("volume '%s' is declared in both volumes and x-volumes", key)
		}
		size, err := provisionedSize(value)
		if err != nil {
			return nil, fmt.Errorf("x-volumes.%s: %w", key, err)
		}
		provisioned[key] = size
		volumes[key] = object{}
	}
	if req.YAML == "" {
		if err := schema.Validate(model); err != nil {
			return nil, err
		}
		if err := validation.Validate(model); err != nil {
			return nil, err
		}
	}
	if len(profiles) == 0 && env["COMPOSE_PROFILES"] != "" {
		profiles = strings.Split(env["COMPOSE_PROFILES"], ",")
	}
	opts := &loader.Options{Profiles: profiles, SkipConsistencyCheck: req.YAML != ""}
	opts.SetProjectName(name, true)
	project, err := loader.ModelToProject(model, opts, types.ConfigDetails{WorkingDir: directory, Environment: env})
	if err != nil {
		return nil, err
	}
	return convert(project, model, provisioned, secrets)
}

func mapping(v any) object { m, _ := v.(map[string]any); return m }

func secretSource(name string, raw object) (object, error) {
	if raw["external"] == true {
		return nil, fmt.Errorf("secret '%s': external secrets are not supported", name)
	}
	command, hasCommand := raw["x-command"]
	driver, hasDriver := raw["driver"]
	_, hasDriverOptions := raw["driver_opts"]
	if hasCommand || hasDriver {
		if hasCommand && (hasDriver || hasDriverOptions) {
			return nil, fmt.Errorf("secret '%s': x-command cannot be combined with driver or driver_opts", name)
		}
		if raw["file"] != nil || raw["environment"] != nil {
			if hasCommand {
				return nil, fmt.Errorf("secret '%s': x-command cannot be combined with file or environment", name)
			}
			return nil, fmt.Errorf("secret '%s': a secret using a driver cannot also define file or environment", name)
		}
		if hasDriver {
			if driver != "exec" {
				return nil, fmt.Errorf("secret '%s': unsupported driver '%v'", name, driver)
			}
			command = mapping(raw["driver_opts"])["command"]
		}
		value, ok := command.(string)
		if !ok || value == "" {
			return nil, fmt.Errorf("secret '%s': command must be a non-empty string", name)
		}
		return object{"Command": value}, nil
	}
	file, fileOK := raw["file"].(string)
	variable, envOK := raw["environment"].(string)
	if fileOK == envOK {
		return nil, fmt.Errorf("secret '%s' must define exactly one of file or environment", name)
	}
	if fileOK {
		return object{"File": file}, nil
	}
	return object{"Environment": variable}, nil
}
