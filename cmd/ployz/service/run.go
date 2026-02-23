package service

import (
	"fmt"
	"os"
	"strconv"
	"strings"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	"ployz/pkg/sdk/types"

	"github.com/spf13/cobra"
	"gopkg.in/yaml.v3"
)

const deployEventsBufferCapacity = 256

type composeServiceFile struct {
	Name     string                        `yaml:"name"`
	Services map[string]composeServiceSpec `yaml:"services"`
}

type composeServiceSpec struct {
	Image       string            `yaml:"image"`
	Ports       []string          `yaml:"ports,omitempty"`
	Environment map[string]string `yaml:"environment,omitempty"`
	Deploy      composeDeploySpec `yaml:"deploy"`
}

type composeDeploySpec struct {
	Replicas int `yaml:"replicas"`
}

type deployPlanCounts struct {
	Create          int
	NeedsSpecUpdate int
	NeedsUpdate     int
	NeedsRecreate   int
	Remove          int
	UpToDate        int
}

func (c deployPlanCounts) ChangeCount() int {
	return c.Create + c.NeedsSpecUpdate + c.NeedsUpdate + c.NeedsRecreate + c.Remove
}

func runCmd() *cobra.Command {
	var cf cmdutil.ClusterFlags
	var image string
	var replicas int
	var ports []string
	var envVars []string
	var dryRun bool

	cmd := &cobra.Command{
		Use:   "run <name>",
		Short: "Run or update a single-image service",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			clusterName, api, cl, err := cf.DialService(cmd.Context())
			if err != nil {
				return err
			}

			name := strings.TrimSpace(args[0])
			if name == "" {
				return fmt.Errorf("service name is required")
			}
			if strings.ContainsAny(name, " \t\n\r") {
				return fmt.Errorf("service name %q must not contain whitespace", name)
			}

			composeSpec, err := buildServiceComposeSpec(name, image, replicas, ports, envVars)
			if err != nil {
				return err
			}

			if dryRun {
				plan, err := api.PlanDeploy(cmd.Context(), cl.Network, name, composeSpec)
				if err != nil {
					return err
				}
				counts := countPlanActions(plan)
				fmt.Println(ui.InfoMsg("planned service %s for cluster %s", ui.Accent(name), ui.Accent(clusterName)))
				fmt.Print(ui.KeyValues("  ",
					ui.KV("namespace", plan.Namespace),
					ui.KV("deploy", plan.DeployID),
					ui.KV("tiers", strconv.Itoa(len(plan.Tiers))),
					ui.KV("create", strconv.Itoa(counts.Create)),
					ui.KV("spec update", strconv.Itoa(counts.NeedsSpecUpdate)),
					ui.KV("update", strconv.Itoa(counts.NeedsUpdate)),
					ui.KV("recreate", strconv.Itoa(counts.NeedsRecreate)),
					ui.KV("remove", strconv.Itoa(counts.Remove)),
					ui.KV("up to date", strconv.Itoa(counts.UpToDate)),
				))
				if counts.ChangeCount() == 0 {
					fmt.Println(ui.SuccessMsg("no changes needed"))
				}
				return nil
			}

			fmt.Fprintln(os.Stderr, ui.InfoMsg("deploying service %s to cluster %s", ui.Accent(name), ui.Accent(clusterName)))
			events := make(chan types.DeployProgressEvent, deployEventsBufferCapacity)
			done := make(chan struct{})
			go func() {
				defer close(done)
				for ev := range events {
					line, ok := formatDeployEvent(ev)
					if !ok {
						continue
					}
					fmt.Fprintln(os.Stderr, line)
				}
			}()

			result, err := api.ApplyDeploy(cmd.Context(), cl.Network, name, composeSpec, events)
			close(events)
			<-done
			if err != nil {
				if msg := strings.TrimSpace(result.ErrorMessage); msg != "" {
					fmt.Fprintln(os.Stderr, ui.ErrorMsg("%s", msg))
				}
				if phase := strings.TrimSpace(result.ErrorPhase); phase != "" {
					fmt.Fprintln(os.Stderr, ui.WarnMsg("deploy phase: %s", phase))
				}
				return err
			}

			fmt.Println(ui.SuccessMsg("service %s deployed to cluster %s", ui.Accent(name), ui.Accent(clusterName)))
			fmt.Print(ui.KeyValues("  ",
				ui.KV("namespace", result.Namespace),
				ui.KV("deploy", result.DeployID),
				ui.KV("status", result.Status),
				ui.KV("tiers", strconv.Itoa(len(result.Tiers))),
			))
			return nil
		},
	}

	cf.Bind(cmd)
	cmd.Flags().StringVar(&image, "image", "", "Container image reference")
	cmd.Flags().IntVar(&replicas, "replicas", 1, "Replica count (min 1)")
	cmd.Flags().StringArrayVar(&ports, "port", nil, "Published port mapping (host:container), repeatable")
	cmd.Flags().StringArrayVar(&envVars, "env", nil, "Environment variable (KEY=VALUE), repeatable")
	cmd.Flags().BoolVar(&dryRun, "dry-run", false, "Plan only; do not apply changes")
	return cmd
}

func buildServiceComposeSpec(name, image string, replicas int, ports, envVars []string) ([]byte, error) {
	name = strings.TrimSpace(name)
	if name == "" {
		return nil, fmt.Errorf("service name is required")
	}

	image = strings.TrimSpace(image)
	if image == "" {
		return nil, fmt.Errorf("image is required")
	}

	if replicas < 1 {
		return nil, fmt.Errorf("replicas must be at least 1")
	}

	parsedPorts, err := normalizePortFlags(ports)
	if err != nil {
		return nil, err
	}
	parsedEnv, err := parseEnvFlags(envVars)
	if err != nil {
		return nil, err
	}

	spec := composeServiceFile{
		Name: name,
		Services: map[string]composeServiceSpec{
			name: {
				Image:       image,
				Ports:       parsedPorts,
				Environment: parsedEnv,
				Deploy: composeDeploySpec{
					Replicas: replicas,
				},
			},
		},
	}
	encoded, err := yaml.Marshal(spec)
	if err != nil {
		return nil, fmt.Errorf("encode compose spec: %w", err)
	}
	return encoded, nil
}

func normalizePortFlags(ports []string) ([]string, error) {
	out := make([]string, 0, len(ports))
	for _, value := range ports {
		trimmed := strings.TrimSpace(value)
		if trimmed == "" {
			return nil, fmt.Errorf("port entries must not be empty")
		}
		out = append(out, trimmed)
	}
	return out, nil
}

func parseEnvFlags(values []string) (map[string]string, error) {
	if len(values) == 0 {
		return nil, nil
	}

	env := make(map[string]string, len(values))
	for _, value := range values {
		pair := strings.TrimSpace(value)
		if pair == "" {
			return nil, fmt.Errorf("environment entries must not be empty")
		}

		key, rawValue, ok := strings.Cut(pair, "=")
		if !ok {
			return nil, fmt.Errorf("environment entry %q must be KEY=VALUE", pair)
		}
		key = strings.TrimSpace(key)
		if key == "" {
			return nil, fmt.Errorf("environment entry %q has empty key", pair)
		}
		env[key] = rawValue
	}
	return env, nil
}

func countPlanActions(plan types.DeployPlan) deployPlanCounts {
	counts := deployPlanCounts{}
	for _, tier := range plan.Tiers {
		for _, service := range tier.Services {
			counts.Create += len(service.Create)
			counts.NeedsSpecUpdate += len(service.NeedsSpecUpdate)
			counts.NeedsUpdate += len(service.NeedsUpdate)
			counts.NeedsRecreate += len(service.NeedsRecreate)
			counts.Remove += len(service.Remove)
			counts.UpToDate += len(service.UpToDate)
		}
	}
	return counts
}

func formatDeployEvent(ev types.DeployProgressEvent) (string, bool) {
	switch ev.Type {
	case "tier_started":
		return ui.InfoMsg("tier %d started: %s", ev.Tier+1, strings.TrimSpace(ev.Message)), true
	case "tier_complete":
		return ui.SuccessMsg("tier %d complete: %s", ev.Tier+1, strings.TrimSpace(ev.Message)), true
	case "image_pulled":
		if strings.TrimSpace(ev.Message) == "" {
			return "", false
		}
		return ui.InfoMsg("pulled image %s", ui.Accent(ev.Message)), true
	case "container_created":
		return ui.InfoMsg("created container %s", eventTarget(ev)), true
	case "container_started":
		return ui.SuccessMsg("started container %s", eventTarget(ev)), true
	case "container_removed":
		return ui.InfoMsg("removed container %s", eventTarget(ev)), true
	case "container_updated":
		return ui.InfoMsg("updated container %s", eventTarget(ev)), true
	case "spec_updated":
		return ui.InfoMsg("updated spec for %s", eventTarget(ev)), true
	case "health_check_passed":
		return ui.SuccessMsg("healthy container %s", eventTarget(ev)), true
	case "rollback_started":
		return ui.WarnMsg("rollback started for tier %d", ev.Tier+1), true
	case "deploy_complete":
		if strings.TrimSpace(ev.Message) == "" {
			return ui.SuccessMsg("deploy complete"), true
		}
		return ui.SuccessMsg("deploy complete %s", ui.Accent(ev.Message)), true
	case "deploy_failed":
		return ui.ErrorMsg("deploy failed: %s", strings.TrimSpace(ev.Message)), true
	default:
		return "", false
	}
}

func eventTarget(ev types.DeployProgressEvent) string {
	container := strings.TrimSpace(ev.Container)
	machineID := strings.TrimSpace(ev.MachineID)
	switch {
	case container != "" && machineID != "":
		return ui.Accent(container) + " on " + ui.Accent(machineID)
	case container != "":
		return ui.Accent(container)
	case machineID != "":
		return ui.Accent(machineID)
	default:
		return "-"
	}
}
