package deploy

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"sort"
	"strings"
	"sync"
	"time"

	"ployz/internal/check"
	"ployz/internal/network"
)

const (
	ownerHeartbeatInterval = 5 * time.Second

	labelNamespace = "ployz.namespace"
	labelService   = "ployz.service"
	labelDeployID  = "ployz.deploy_id"
	labelMachineID = "ployz.machine_id"

	defaultUpdateParallelism = 1
)

// ApplyPlan executes a deploy plan tier-by-tier against runtime + stores.
//
// The events channel is optional and never closed by ApplyPlan. Events are
// sent with non-blocking writes and may be dropped if the channel is full.
func ApplyPlan(
	ctx context.Context,
	rt network.ContainerRuntime,
	stores Stores,
	health HealthChecker,
	stateReader StateReader,
	plan DeployPlan,
	machineID string,
	clock network.Clock,
	events chan<- ProgressEvent,
) (result ApplyResult, retErr error) {
	check.Assert(rt != nil, "ApplyPlan: container runtime must not be nil")
	check.Assert(stores.Containers != nil, "ApplyPlan: container store must not be nil")
	check.Assert(stores.Deployments != nil, "ApplyPlan: deployment store must not be nil")
	check.Assert(health != nil, "ApplyPlan: health checker must not be nil")
	check.Assert(stateReader != nil, "ApplyPlan: state reader must not be nil")
	check.Assert(clock != nil, "ApplyPlan: clock must not be nil")

	result = ApplyResult{
		Namespace: plan.Namespace,
		DeployID:  plan.DeployID,
		Tiers:     make([]TierResult, 0, len(plan.Tiers)),
	}

	cancelHeartbeat, err := preFlight(ctx, stores, plan, machineID, clock)
	if err != nil {
		return result, fmt.Errorf("deploy pre-flight: %w", err)
	}
	defer cancelHeartbeat()

	finalStatus := DeployFailed
	defer func() {
		if err := postFlight(ctx, stores, plan, finalStatus, clock); err != nil {
			if retErr == nil {
				retErr = fmt.Errorf("deploy post-flight: %w", err)
				return
			}
			retErr = fmt.Errorf("%w; deploy post-flight: %v", retErr, err)
		}
	}()

	for tierIdx, tier := range plan.Tiers {
		if err := ctx.Err(); err != nil {
			tierName := tierDisplayName(tier, tierIdx)
			retErr = decorateDeployError(err, DeployErrorPhaseExecute, plan.Namespace, tierIdx, tierName, result.Tiers)
			emit(events, ProgressEvent{Type: "deploy_failed", Tier: tierIdx, Message: retErr.Error()})
			return result, retErr
		}

		tierName := tierDisplayName(tier, tierIdx)
		emit(events, ProgressEvent{Type: "tier_started", Tier: tierIdx, Message: tierName})

		if err := checkOwnership(ctx, stores, plan, machineID, tierIdx, tierName); err != nil {
			retErr = decorateDeployError(err, DeployErrorPhaseOwnership, plan.Namespace, tierIdx, tierName, result.Tiers)
			emit(events, ProgressEvent{Type: "deploy_failed", Tier: tierIdx, Message: retErr.Error()})
			return result, retErr
		}

		if err := prePullTier(ctx, rt, tier, tierIdx, machineID, events); err != nil {
			retErr = decorateDeployError(err, DeployErrorPhasePrePull, plan.Namespace, tierIdx, tierName, result.Tiers)
			emit(events, ProgressEvent{Type: "deploy_failed", Tier: tierIdx, Message: retErr.Error()})
			return result, retErr
		}

		tierResult, err := executeTier(ctx, rt, stores, health, tier, tierIdx, plan, machineID, clock, events)
		if err != nil {
			if tierResult.Status == 0 {
				tierResult.Status = TierFailed
			}
			result.Tiers = append(result.Tiers, tierResult)
			retErr = decorateDeployError(err, DeployErrorPhaseExecute, plan.Namespace, tierIdx, tierName, result.Tiers)
			emit(events, ProgressEvent{Type: "deploy_failed", Tier: tierIdx, Message: retErr.Error()})
			return result, retErr
		}

		postconditionRows, err := assertPostcondition(ctx, stateReader, tier, tierIdx, plan, machineID)
		tierResult.Containers = postconditionRows
		if err != nil {
			tierResult.Status = TierFailed
			result.Tiers = append(result.Tiers, tierResult)
			retErr = decorateDeployError(err, DeployErrorPhasePostcondition, plan.Namespace, tierIdx, tierName, result.Tiers)
			emit(events, ProgressEvent{Type: "deploy_failed", Tier: tierIdx, Message: retErr.Error()})
			return result, retErr
		}

		tierResult.Status = TierCompleted
		result.Tiers = append(result.Tiers, tierResult)
		emit(events, ProgressEvent{Type: "tier_complete", Tier: tierIdx, Message: tierName})
	}

	finalStatus = DeploySucceeded
	emit(events, ProgressEvent{Type: "deploy_complete", Message: plan.DeployID})
	return result, nil
}

type rollbackAction struct {
	description string
	run         func(context.Context) error
}

type healthTarget struct {
	service   string
	container string
	check     HealthCheck
}

// preFlight writes/updates the deployment row and starts ownership heartbeats.
func preFlight(
	ctx context.Context,
	stores Stores,
	plan DeployPlan,
	machineID string,
	clock network.Clock,
) (cancel func(), err error) {
	now := clock.Now().UTC().Format(time.RFC3339Nano)
	specJSONBytes, err := json.Marshal(plan)
	if err != nil {
		return nil, fmt.Errorf("marshal deploy plan: %w", err)
	}

	row := DeploymentRow{
		ID:             plan.DeployID,
		Namespace:      plan.Namespace,
		SpecJSON:       string(specJSONBytes),
		Labels:         map[string]string{},
		Status:         DeployInProgress,
		Owner:          machineID,
		OwnerHeartbeat: now,
		MachineIDs:     planMachineIDs(plan),
		Version:        1,
		CreatedAt:      now,
		UpdatedAt:      now,
	}

	existing, ok, err := stores.Deployments.GetDeployment(ctx, plan.DeployID)
	if err != nil {
		return nil, fmt.Errorf("read deployment row %q: %w", plan.DeployID, err)
	}
	if ok {
		row.CreatedAt = existing.CreatedAt
		if err := stores.Deployments.UpdateDeployment(ctx, row); err != nil {
			return nil, fmt.Errorf("update deployment row %q: %w", plan.DeployID, err)
		}
	} else {
		if err := stores.Deployments.InsertDeployment(ctx, row); err != nil {
			return nil, fmt.Errorf("insert deployment row %q: %w", plan.DeployID, err)
		}
	}

	if err := stores.Deployments.AcquireOwnership(ctx, plan.DeployID, machineID, now); err != nil {
		return nil, fmt.Errorf("acquire deploy ownership %q: %w", plan.DeployID, err)
	}

	heartbeatCtx, heartbeatCancel := context.WithCancel(ctx)
	var wg sync.WaitGroup
	wg.Add(1)
	go func() {
		defer wg.Done()
		ticker := time.NewTicker(ownerHeartbeatInterval)
		defer ticker.Stop()

		for {
			select {
			case <-heartbeatCtx.Done():
				return
			case <-ticker.C:
				hbNow := clock.Now().UTC().Format(time.RFC3339Nano)
				if err := stores.Deployments.BumpOwnershipHeartbeat(heartbeatCtx, plan.DeployID, machineID, hbNow); err != nil {
					return
				}
			}
		}
	}()

	return func() {
		heartbeatCancel()
		wg.Wait()
	}, nil
}

// checkOwnership verifies deployment ownership for this machine.
func checkOwnership(
	ctx context.Context,
	stores Stores,
	plan DeployPlan,
	machineID string,
	tier int,
	tierName string,
) error {
	if err := stores.Deployments.CheckOwnership(ctx, plan.DeployID, machineID); err != nil {
		return &DeployError{
			Namespace: plan.Namespace,
			Phase:     DeployErrorPhaseOwnership,
			Tier:      tier,
			TierName:  tierName,
			Message:   err.Error(),
		}
	}
	return nil
}

// prePullTier pulls all unique images for local create/recreate operations.
func prePullTier(
	ctx context.Context,
	rt network.ContainerRuntime,
	tier Tier,
	tierIdx int,
	machineID string,
	events chan<- ProgressEvent,
) error {
	imagesSet := make(map[string]struct{})
	for _, svc := range tier.Services {
		for _, entry := range filterEntriesForMachine(svc.Create, machineID) {
			if img := strings.TrimSpace(entry.Spec.Image); img != "" {
				imagesSet[img] = struct{}{}
			}
		}
		for _, entry := range filterEntriesForMachine(svc.NeedsRecreate, machineID) {
			if img := strings.TrimSpace(entry.Spec.Image); img != "" {
				imagesSet[img] = struct{}{}
			}
		}
	}

	images := make([]string, 0, len(imagesSet))
	for image := range imagesSet {
		images = append(images, image)
	}
	sort.Strings(images)

	for _, image := range images {
		if err := rt.ImagePull(ctx, image); err != nil {
			return &DeployError{
				Phase:   DeployErrorPhasePrePull,
				Tier:    tierIdx,
				Message: fmt.Sprintf("pull image %q: %v", image, err),
			}
		}
		emit(events, ProgressEvent{Type: "image_pulled", Tier: tierIdx, Message: image})
	}

	return nil
}

// executeTier applies one tier for the local machine only.
func executeTier(
	ctx context.Context,
	rt network.ContainerRuntime,
	stores Stores,
	health HealthChecker,
	tier Tier,
	tierIdx int,
	plan DeployPlan,
	machineID string,
	clock network.Clock,
	events chan<- ProgressEvent,
) (TierResult, error) {
	tierName := tierDisplayName(tier, tierIdx)
	result := TierResult{
		Name:   tierName,
		Status: TierCompleted,
	}

	rollbackActions := make([]rollbackAction, 0)
	healthTargets := make([]healthTarget, 0)
	healthChecked := make(map[string]bool)

	for _, service := range tier.Services {
		if err := ctx.Err(); err != nil {
			return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: err.Error()}
		}

		for _, entry := range filterEntriesForMachine(service.Remove, machineID) {
			if entry.CurrentRow == nil {
				return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("service %s remove %s has nil current row", service.Name, entry.ContainerName)}
			}
			oldRow := *entry.CurrentRow
			oldSpec, err := decodeServiceSpec(oldRow.SpecJSON)
			if err != nil {
				return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("decode current spec for %s: %v", oldRow.ContainerName, err)}
			}

			if err := rt.ContainerStop(ctx, oldRow.ContainerName); err != nil {
				return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("stop container %s: %v", oldRow.ContainerName, err)}
			}
			if err := rt.ContainerRemove(ctx, oldRow.ContainerName, true); err != nil {
				return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("remove container %s: %v", oldRow.ContainerName, err)}
			}
			if err := stores.Containers.DeleteContainer(ctx, oldRow.ID); err != nil {
				return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("delete container row %s: %v", oldRow.ID, err)}
			}

			rollbackActions = append(rollbackActions, rollbackAction{
				description: "restore removed container",
				run: func(ctx context.Context) error {
					cfg := createConfigForSpec(plan.Namespace, oldRow.DeployID, oldRow.MachineID, service.Name, oldRow.ContainerName, oldSpec)
					if err := rt.ContainerCreate(ctx, cfg); err != nil {
						return fmt.Errorf("rollback create container %s: %w", oldRow.ContainerName, err)
					}
					if err := rt.ContainerStart(ctx, oldRow.ContainerName); err != nil {
						return fmt.Errorf("rollback start container %s: %w", oldRow.ContainerName, err)
					}
					if err := stores.Containers.InsertContainer(ctx, oldRow); err != nil {
						return fmt.Errorf("rollback insert container row %s: %w", oldRow.ID, err)
					}
					return nil
				},
			})

			emit(events, ProgressEvent{Type: "container_removed", Tier: tierIdx, Service: service.Name, MachineID: oldRow.MachineID, Container: oldRow.ContainerName})
		}

		for _, entry := range filterEntriesForMachine(service.Create, machineID) {
			now := clock.Now().UTC().Format(time.RFC3339Nano)
			specJSON, err := marshalSpecJSON(entry.Spec)
			if err != nil {
				return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("marshal spec for %s: %v", entry.ContainerName, err)}
			}

			cfg := createConfigForSpec(plan.Namespace, plan.DeployID, entry.MachineID, service.Name, entry.ContainerName, entry.Spec)
			if err := rt.ContainerCreate(ctx, cfg); err != nil {
				return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("create container %s: %v", entry.ContainerName, err)}
			}
			emit(events, ProgressEvent{Type: "container_created", Tier: tierIdx, Service: service.Name, MachineID: entry.MachineID, Container: entry.ContainerName})

			if err := rt.ContainerStart(ctx, entry.ContainerName); err != nil {
				return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("start container %s: %v", entry.ContainerName, err)}
			}
			emit(events, ProgressEvent{Type: "container_started", Tier: tierIdx, Service: service.Name, MachineID: entry.MachineID, Container: entry.ContainerName})

			row := buildContainerRow(plan, service.Name, entry, specJSON, now)
			if err := stores.Containers.InsertContainer(ctx, row); err != nil {
				return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("insert container row %s: %v", row.ID, err)}
			}

			newRow := row
			rollbackActions = append(rollbackActions, rollbackAction{
				description: "remove created container",
				run: func(ctx context.Context) error {
					_ = rt.ContainerStop(ctx, newRow.ContainerName)
					if err := rt.ContainerRemove(ctx, newRow.ContainerName, true); err != nil {
						return fmt.Errorf("rollback remove container %s: %w", newRow.ContainerName, err)
					}
					if err := stores.Containers.DeleteContainer(ctx, newRow.ID); err != nil {
						return fmt.Errorf("rollback delete container row %s: %w", newRow.ID, err)
					}
					return nil
				},
			})

			if service.HealthCheck != nil {
				healthTargets = append(healthTargets, healthTarget{
					service:   service.Name,
					container: entry.ContainerName,
					check:     *service.HealthCheck,
				})
			}
		}

		for _, entry := range filterEntriesForMachine(service.NeedsSpecUpdate, machineID) {
			if entry.CurrentRow == nil {
				return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("service %s spec update %s has nil current row", service.Name, entry.ContainerName)}
			}
			now := clock.Now().UTC().Format(time.RFC3339Nano)
			specJSON, err := marshalSpecJSON(entry.Spec)
			if err != nil {
				return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("marshal spec for %s: %v", entry.ContainerName, err)}
			}

			updated := *entry.CurrentRow
			updated.SpecJSON = specJSON
			updated.UpdatedAt = now
			if updated.Status == "" {
				updated.Status = "running"
			}

			if err := stores.Containers.UpdateContainer(ctx, updated); err != nil {
				return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("update container row %s: %v", updated.ID, err)}
			}

			oldRow := *entry.CurrentRow
			rollbackActions = append(rollbackActions, rollbackAction{
				description: "restore spec metadata",
				run: func(ctx context.Context) error {
					if err := stores.Containers.UpdateContainer(ctx, oldRow); err != nil {
						return fmt.Errorf("rollback restore container row %s: %w", oldRow.ID, err)
					}
					return nil
				},
			})

			emit(events, ProgressEvent{Type: "spec_updated", Tier: tierIdx, Service: service.Name, MachineID: updated.MachineID, Container: updated.ContainerName})
		}

		for _, entry := range filterEntriesForMachine(service.NeedsUpdate, machineID) {
			if entry.CurrentRow == nil {
				return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("service %s update %s has nil current row", service.Name, entry.ContainerName)}
			}
			oldSpec, err := decodeServiceSpec(entry.CurrentRow.SpecJSON)
			if err != nil {
				return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("decode current spec for %s: %v", entry.CurrentRow.ContainerName, err)}
			}

			if err := rt.ContainerUpdate(ctx, entry.CurrentRow.ContainerName, resourceConfigFromSpec(entry.Spec)); err != nil {
				return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("update container resources %s: %v", entry.CurrentRow.ContainerName, err)}
			}

			now := clock.Now().UTC().Format(time.RFC3339Nano)
			specJSON, err := marshalSpecJSON(entry.Spec)
			if err != nil {
				return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("marshal spec for %s: %v", entry.ContainerName, err)}
			}

			updated := *entry.CurrentRow
			updated.SpecJSON = specJSON
			updated.UpdatedAt = now
			if updated.Status == "" {
				updated.Status = "running"
			}

			if err := stores.Containers.UpdateContainer(ctx, updated); err != nil {
				return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("update container row %s: %v", updated.ID, err)}
			}

			oldRow := *entry.CurrentRow
			oldResources := resourceConfigFromSpec(oldSpec)
			rollbackActions = append(rollbackActions, rollbackAction{
				description: "restore resources",
				run: func(ctx context.Context) error {
					if err := rt.ContainerUpdate(ctx, oldRow.ContainerName, oldResources); err != nil {
						return fmt.Errorf("rollback restore resources on %s: %w", oldRow.ContainerName, err)
					}
					if err := stores.Containers.UpdateContainer(ctx, oldRow); err != nil {
						return fmt.Errorf("rollback restore row %s: %w", oldRow.ID, err)
					}
					return nil
				},
			})

			emit(events, ProgressEvent{Type: "container_updated", Tier: tierIdx, Service: service.Name, MachineID: updated.MachineID, Container: updated.ContainerName})
			if service.HealthCheck != nil {
				healthTargets = append(healthTargets, healthTarget{
					service:   service.Name,
					container: updated.ContainerName,
					check:     *service.HealthCheck,
				})
			}
		}

		recreateEntries := filterEntriesForMachine(service.NeedsRecreate, machineID)
		if len(recreateEntries) == 0 {
			continue
		}

		order := DetectUpdateOrder(service, service.UpdateConfig)
		parallelism := service.UpdateConfig.Parallelism
		if parallelism <= 0 {
			parallelism = defaultUpdateParallelism
		}

		for start := 0; start < len(recreateEntries); start += parallelism {
			end := start + parallelism
			if end > len(recreateEntries) {
				end = len(recreateEntries)
			}
			batch := recreateEntries[start:end]

			type recreatePrepared struct {
				entry     PlanEntry
				oldRow    ContainerRow
				oldSpec   ServiceSpec
				newRow    ContainerRow
				newConfig network.ContainerCreateConfig
			}
			prepared := make([]recreatePrepared, 0, len(batch))
			for _, entry := range batch {
				if entry.CurrentRow == nil {
					return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("service %s recreate %s has nil current row", service.Name, entry.ContainerName)}
				}
				oldRow := *entry.CurrentRow
				oldSpec, err := decodeServiceSpec(oldRow.SpecJSON)
				if err != nil {
					return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("decode current spec for %s: %v", oldRow.ContainerName, err)}
				}
				now := clock.Now().UTC().Format(time.RFC3339Nano)
				specJSON, err := marshalSpecJSON(entry.Spec)
				if err != nil {
					return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("marshal spec for %s: %v", entry.ContainerName, err)}
				}
				prepared = append(prepared, recreatePrepared{
					entry:     entry,
					oldRow:    oldRow,
					oldSpec:   oldSpec,
					newRow:    buildContainerRow(plan, service.Name, entry, specJSON, now),
					newConfig: createConfigForSpec(plan.Namespace, plan.DeployID, entry.MachineID, service.Name, entry.ContainerName, entry.Spec),
				})
			}

			if order == updateOrderStopFirst {
				for _, item := range prepared {
					if err := rt.ContainerStop(ctx, item.oldRow.ContainerName); err != nil {
						return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("stop old container %s: %v", item.oldRow.ContainerName, err)}
					}
					if err := rt.ContainerRemove(ctx, item.oldRow.ContainerName, true); err != nil {
						return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("remove old container %s: %v", item.oldRow.ContainerName, err)}
					}
				}
				for _, item := range prepared {
					if err := rt.ContainerCreate(ctx, item.newConfig); err != nil {
						return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("create new container %s: %v", item.entry.ContainerName, err)}
					}
					emit(events, ProgressEvent{Type: "container_created", Tier: tierIdx, Service: service.Name, MachineID: item.entry.MachineID, Container: item.entry.ContainerName})

					if err := rt.ContainerStart(ctx, item.entry.ContainerName); err != nil {
						return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("start new container %s: %v", item.entry.ContainerName, err)}
					}
					emit(events, ProgressEvent{Type: "container_started", Tier: tierIdx, Service: service.Name, MachineID: item.entry.MachineID, Container: item.entry.ContainerName})

					if err := stores.Containers.DeleteContainer(ctx, item.oldRow.ID); err != nil {
						return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("delete old row %s: %v", item.oldRow.ID, err)}
					}
					if err := stores.Containers.InsertContainer(ctx, item.newRow); err != nil {
						return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("insert new row %s: %v", item.newRow.ID, err)}
					}

					rollbackItem := item
					rollbackActions = append(rollbackActions, rollbackAction{
						description: "restore recreated container",
						run: func(ctx context.Context) error {
							_ = rt.ContainerStop(ctx, rollbackItem.newRow.ContainerName)
							if err := rt.ContainerRemove(ctx, rollbackItem.newRow.ContainerName, true); err != nil {
								return fmt.Errorf("rollback remove new container %s: %w", rollbackItem.newRow.ContainerName, err)
							}
							if err := stores.Containers.DeleteContainer(ctx, rollbackItem.newRow.ID); err != nil {
								return fmt.Errorf("rollback delete new row %s: %w", rollbackItem.newRow.ID, err)
							}
							cfg := createConfigForSpec(plan.Namespace, rollbackItem.oldRow.DeployID, rollbackItem.oldRow.MachineID, service.Name, rollbackItem.oldRow.ContainerName, rollbackItem.oldSpec)
							if err := rt.ContainerCreate(ctx, cfg); err != nil {
								return fmt.Errorf("rollback create old container %s: %w", rollbackItem.oldRow.ContainerName, err)
							}
							if err := rt.ContainerStart(ctx, rollbackItem.oldRow.ContainerName); err != nil {
								return fmt.Errorf("rollback start old container %s: %w", rollbackItem.oldRow.ContainerName, err)
							}
							if err := stores.Containers.InsertContainer(ctx, rollbackItem.oldRow); err != nil {
								return fmt.Errorf("rollback insert old row %s: %w", rollbackItem.oldRow.ID, err)
							}
							return nil
						},
					})

					if service.HealthCheck != nil {
						healthTargets = append(healthTargets, healthTarget{
							service:   service.Name,
							container: item.entry.ContainerName,
							check:     *service.HealthCheck,
						})
					}
				}
				continue
			}

			// start-first
			for _, item := range prepared {
				if err := rt.ContainerCreate(ctx, item.newConfig); err != nil {
					return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("create new container %s: %v", item.entry.ContainerName, err)}
				}
				emit(events, ProgressEvent{Type: "container_created", Tier: tierIdx, Service: service.Name, MachineID: item.entry.MachineID, Container: item.entry.ContainerName})

				if err := rt.ContainerStart(ctx, item.entry.ContainerName); err != nil {
					return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("start new container %s: %v", item.entry.ContainerName, err)}
				}
				emit(events, ProgressEvent{Type: "container_started", Tier: tierIdx, Service: service.Name, MachineID: item.entry.MachineID, Container: item.entry.ContainerName})

				rollbackItem := item
				rollbackIndex := len(rollbackActions)
				rollbackActions = append(rollbackActions, rollbackAction{
					description: "remove started replacement container",
					run: func(ctx context.Context) error {
						_ = rt.ContainerStop(ctx, rollbackItem.newRow.ContainerName)
						if err := rt.ContainerRemove(ctx, rollbackItem.newRow.ContainerName, true); err != nil {
							return fmt.Errorf("rollback remove new container %s: %w", rollbackItem.newRow.ContainerName, err)
						}
						_ = stores.Containers.DeleteContainer(ctx, rollbackItem.newRow.ID)
						return nil
					},
				})

				if service.HealthCheck != nil {
					if err := health.WaitHealthy(ctx, item.entry.ContainerName, *service.HealthCheck); err != nil {
						rbErr := rollbackTierActions(ctx, rollbackActions, tierIdx, events)
						msg := fmt.Sprintf("container %s health failed: %v", item.entry.ContainerName, err)
						if rbErr != nil {
							msg = msg + "; rollback: " + rbErr.Error()
						}
						result.Status = TierRolledBack
						return result, &DeployError{Phase: DeployErrorPhaseHealth, Tier: tierIdx, TierName: tierName, Message: msg}
					}
					emit(events, ProgressEvent{Type: "health_check_passed", Tier: tierIdx, Service: service.Name, MachineID: item.entry.MachineID, Container: item.entry.ContainerName})
					healthChecked[item.entry.ContainerName] = true
				}

				if err := rt.ContainerStop(ctx, item.oldRow.ContainerName); err != nil {
					return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("stop old container %s: %v", item.oldRow.ContainerName, err)}
				}
				if err := rt.ContainerRemove(ctx, item.oldRow.ContainerName, true); err != nil {
					return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("remove old container %s: %v", item.oldRow.ContainerName, err)}
				}

				if err := stores.Containers.DeleteContainer(ctx, item.oldRow.ID); err != nil {
					return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("delete old row %s: %v", item.oldRow.ID, err)}
				}
				if err := stores.Containers.InsertContainer(ctx, item.newRow); err != nil {
					return result, &DeployError{Phase: DeployErrorPhaseExecute, Tier: tierIdx, TierName: tierName, Message: fmt.Sprintf("insert new row %s: %v", item.newRow.ID, err)}
				}

				rollbackActions[rollbackIndex] = rollbackAction{
					description: "restore replaced container",
					run: func(ctx context.Context) error {
						_ = rt.ContainerStop(ctx, rollbackItem.newRow.ContainerName)
						if err := rt.ContainerRemove(ctx, rollbackItem.newRow.ContainerName, true); err != nil {
							return fmt.Errorf("rollback remove new container %s: %w", rollbackItem.newRow.ContainerName, err)
						}
						if err := stores.Containers.DeleteContainer(ctx, rollbackItem.newRow.ID); err != nil {
							return fmt.Errorf("rollback delete new row %s: %w", rollbackItem.newRow.ID, err)
						}
						cfg := createConfigForSpec(plan.Namespace, rollbackItem.oldRow.DeployID, rollbackItem.oldRow.MachineID, service.Name, rollbackItem.oldRow.ContainerName, rollbackItem.oldSpec)
						if err := rt.ContainerCreate(ctx, cfg); err != nil {
							return fmt.Errorf("rollback create old container %s: %w", rollbackItem.oldRow.ContainerName, err)
						}
						if err := rt.ContainerStart(ctx, rollbackItem.oldRow.ContainerName); err != nil {
							return fmt.Errorf("rollback start old container %s: %w", rollbackItem.oldRow.ContainerName, err)
						}
						if err := stores.Containers.InsertContainer(ctx, rollbackItem.oldRow); err != nil {
							return fmt.Errorf("rollback insert old row %s: %w", rollbackItem.oldRow.ID, err)
						}
						return nil
					},
				}
			}
		}
	}

	for _, target := range healthTargets {
		if healthChecked[target.container] {
			continue
		}
		if err := health.WaitHealthy(ctx, target.container, target.check); err != nil {
			rbErr := rollbackTierActions(ctx, rollbackActions, tierIdx, events)
			msg := fmt.Sprintf("container %s health failed: %v", target.container, err)
			if rbErr != nil {
				msg = msg + "; rollback: " + rbErr.Error()
			}
			result.Status = TierRolledBack
			return result, &DeployError{Phase: DeployErrorPhaseHealth, Tier: tierIdx, TierName: tierName, Message: msg}
		}
		emit(events, ProgressEvent{Type: "health_check_passed", Tier: tierIdx, Service: target.service, MachineID: machineID, Container: target.container})
	}

	return result, nil
}

func rollbackTierActions(ctx context.Context, actions []rollbackAction, tierIdx int, events chan<- ProgressEvent) error {
	if len(actions) == 0 {
		return nil
	}
	emit(events, ProgressEvent{Type: "rollback_started", Tier: tierIdx})

	var firstErr error
	for i := len(actions) - 1; i >= 0; i-- {
		if err := actions[i].run(ctx); err != nil && firstErr == nil {
			firstErr = fmt.Errorf("%s: %w", actions[i].description, err)
		}
	}
	return firstErr
}

func assertPostcondition(
	ctx context.Context,
	stateReader StateReader,
	tier Tier,
	tierIdx int,
	plan DeployPlan,
	machineID string,
) ([]ContainerResult, error) {
	expected := expectedTierContainers(tier, machineID)
	if len(expected) == 0 {
		return nil, nil
	}

	actual, err := stateReader.ReadMachineState(ctx, machineID, plan.Namespace)
	if err != nil {
		return nil, &DeployError{
			Namespace: plan.Namespace,
			Phase:     DeployErrorPhasePostcondition,
			Tier:      tierIdx,
			TierName:  tierDisplayName(tier, tierIdx),
			Message:   fmt.Sprintf("read machine state: %v", err),
		}
	}

	rows, mismatches := compareTierState(actual, expected)
	if mismatches == 0 {
		return rows, nil
	}

	return rows, &DeployError{
		Namespace: plan.Namespace,
		Phase:     DeployErrorPhasePostcondition,
		Tier:      tierIdx,
		TierName:  tierDisplayName(tier, tierIdx),
		Message:   "container state mismatch",
		Tiers: []TierResult{{
			Name:       tierDisplayName(tier, tierIdx),
			Status:     TierFailed,
			Containers: rows,
		}},
	}
}

// postFlight finalizes deployment status and releases ownership.
func postFlight(
	ctx context.Context,
	stores Stores,
	plan DeployPlan,
	status DeployPhase,
	clock network.Clock,
) error {
	row, ok, err := stores.Deployments.GetDeployment(ctx, plan.DeployID)
	if err != nil {
		return fmt.Errorf("read deployment row %q: %w", plan.DeployID, err)
	}
	if !ok {
		return fmt.Errorf("deployment row %q not found", plan.DeployID)
	}

	row.Status = status
	row.Owner = ""
	row.OwnerHeartbeat = ""
	row.UpdatedAt = clock.Now().UTC().Format(time.RFC3339Nano)
	if err := stores.Deployments.UpdateDeployment(ctx, row); err != nil {
		return fmt.Errorf("update deployment row %q: %w", plan.DeployID, err)
	}
	if err := stores.Deployments.ReleaseOwnership(ctx, plan.DeployID); err != nil {
		return fmt.Errorf("release deployment ownership %q: %w", plan.DeployID, err)
	}
	return nil
}

// specToCreateConfig converts a ServiceSpec into runtime create config.
func specToCreateConfig(name string, spec ServiceSpec, networkMode string) network.ContainerCreateConfig {
	cmd := make([]string, 0, len(spec.Entrypoint)+len(spec.Command))
	cmd = append(cmd, spec.Entrypoint...)
	cmd = append(cmd, spec.Command...)
	if len(cmd) == 0 {
		cmd = nil
	}

	mounts := make([]network.Mount, 0, len(spec.Mounts))
	for _, m := range spec.Mounts {
		mounts = append(mounts, network.Mount{Source: m.Source, Target: m.Target, ReadOnly: m.ReadOnly})
	}

	ports := make([]network.PortBinding, 0, len(spec.Ports))
	for _, p := range spec.Ports {
		ports = append(ports, network.PortBinding{HostPort: p.HostPort, ContainerPort: p.ContainerPort, Protocol: p.Protocol})
	}

	labels := make(map[string]string, len(spec.Labels))
	for key, value := range spec.Labels {
		labels[key] = value
	}

	var healthConfig *network.HealthCheckConfig
	if spec.HealthCheck != nil {
		healthConfig = &network.HealthCheckConfig{
			Test:        append([]string(nil), spec.HealthCheck.Test...),
			Interval:    spec.HealthCheck.Interval,
			Timeout:     spec.HealthCheck.Timeout,
			Retries:     spec.HealthCheck.Retries,
			StartPeriod: spec.HealthCheck.StartPeriod,
		}
	}

	return network.ContainerCreateConfig{
		Name:          name,
		Image:         spec.Image,
		Cmd:           cmd,
		Env:           append([]string(nil), spec.Environment...),
		NetworkMode:   networkMode,
		Mounts:        mounts,
		Ports:         ports,
		Labels:        labels,
		RestartPolicy: spec.RestartPolicy,
		HealthCheck:   healthConfig,
	}
}

// emit sends a progress event if events is non-nil.
// The send is non-blocking; events are dropped if the channel is full.
func emit(events chan<- ProgressEvent, ev ProgressEvent) {
	if events == nil {
		return
	}
	select {
	case events <- ev:
	default:
	}
}

func decorateDeployError(err error, phase DeployErrorPhase, namespace string, tier int, tierName string, tiers []TierResult) error {
	var de *DeployError
	if errors.As(err, &de) {
		out := *de
		if out.Namespace == "" {
			out.Namespace = namespace
		}
		if !out.Phase.IsValid() {
			out.Phase = phase
		}
		if out.TierName == "" {
			out.Tier = tier
			out.TierName = tierName
		}
		if len(out.Tiers) == 0 {
			out.Tiers = cloneTierResults(tiers)
		}
		return &out
	}
	return &DeployError{
		Namespace: namespace,
		Phase:     phase,
		Tier:      tier,
		TierName:  tierName,
		Tiers:     cloneTierResults(tiers),
		Message:   err.Error(),
	}
}

func cloneTierResults(in []TierResult) []TierResult {
	out := make([]TierResult, 0, len(in))
	for _, tier := range in {
		copied := tier
		copied.Containers = append([]ContainerResult(nil), tier.Containers...)
		out = append(out, copied)
	}
	return out
}

func tierDisplayName(tier Tier, tierIdx int) string {
	if len(tier.Services) == 0 {
		return fmt.Sprintf("tier-%d", tierIdx)
	}
	names := make([]string, 0, len(tier.Services))
	for _, service := range tier.Services {
		names = append(names, service.Name)
	}
	return strings.Join(names, ",")
}

func filterEntriesForMachine(entries []PlanEntry, machineID string) []PlanEntry {
	out := make([]PlanEntry, 0, len(entries))
	for _, entry := range entries {
		if entry.MachineID != machineID {
			continue
		}
		out = append(out, entry)
	}
	return out
}

func createConfigForSpec(namespace, deployID, machineID, service, containerName string, spec ServiceSpec) network.ContainerCreateConfig {
	cfg := specToCreateConfig(containerName, spec, namespace)
	cfg.Labels = mergeManagedLabels(cfg.Labels, namespace, service, deployID, machineID)
	return cfg
}

func mergeManagedLabels(base map[string]string, namespace, service, deployID, machineID string) map[string]string {
	out := make(map[string]string, len(base)+4)
	for key, value := range base {
		out[key] = value
	}
	out[labelNamespace] = namespace
	out[labelService] = service
	out[labelDeployID] = deployID
	out[labelMachineID] = machineID
	return out
}

func marshalSpecJSON(spec ServiceSpec) (string, error) {
	data, err := json.Marshal(canonicalSpec(spec))
	if err != nil {
		return "", err
	}
	return string(data), nil
}

func buildContainerRow(plan DeployPlan, service string, entry PlanEntry, specJSON, now string) ContainerRow {
	return ContainerRow{
		ID:            containerRowID(plan.DeployID, entry.ContainerName),
		Namespace:     plan.Namespace,
		DeployID:      plan.DeployID,
		Service:       service,
		MachineID:     entry.MachineID,
		ContainerName: entry.ContainerName,
		SpecJSON:      specJSON,
		Status:        "running",
		Version:       1,
		CreatedAt:     now,
		UpdatedAt:     now,
	}
}

func containerRowID(deployID, containerName string) string {
	return deployID + "/" + containerName
}

func resourceConfigFromSpec(spec ServiceSpec) network.ResourceConfig {
	if spec.Resources == nil {
		return network.ResourceConfig{}
	}
	return network.ResourceConfig{
		CPULimit:    spec.Resources.CPULimit,
		MemoryLimit: spec.Resources.MemoryLimit,
	}
}

func planMachineIDs(plan DeployPlan) []string {
	idsSet := make(map[string]struct{})
	for _, tier := range plan.Tiers {
		for _, service := range tier.Services {
			for _, entry := range service.UpToDate {
				idsSet[entry.MachineID] = struct{}{}
			}
			for _, entry := range service.NeedsSpecUpdate {
				idsSet[entry.MachineID] = struct{}{}
			}
			for _, entry := range service.NeedsUpdate {
				idsSet[entry.MachineID] = struct{}{}
			}
			for _, entry := range service.NeedsRecreate {
				idsSet[entry.MachineID] = struct{}{}
			}
			for _, entry := range service.Create {
				idsSet[entry.MachineID] = struct{}{}
			}
			for _, entry := range service.Remove {
				idsSet[entry.MachineID] = struct{}{}
			}
		}
	}

	ids := make([]string, 0, len(idsSet))
	for id := range idsSet {
		ids = append(ids, id)
	}
	sort.Strings(ids)
	return ids
}

func expectedTierContainers(tier Tier, machineID string) []ContainerResult {
	type expectedContainer struct {
		machineID string
		name      string
		image     string
	}

	byName := make(map[string]expectedContainer)
	add := func(entry PlanEntry) {
		if entry.MachineID != machineID {
			return
		}
		if entry.ContainerName == "" {
			return
		}
		image := strings.TrimSpace(entry.Spec.Image)
		if image == "" && entry.CurrentRow != nil {
			if currentSpec, err := decodeServiceSpec(entry.CurrentRow.SpecJSON); err == nil {
				image = currentSpec.Image
			}
		}
		if image == "" {
			return
		}
		byName[entry.ContainerName] = expectedContainer{
			machineID: entry.MachineID,
			name:      entry.ContainerName,
			image:     image,
		}
	}

	for _, service := range tier.Services {
		for _, entry := range service.UpToDate {
			add(entry)
		}
		for _, entry := range service.NeedsSpecUpdate {
			add(entry)
		}
		for _, entry := range service.NeedsUpdate {
			add(entry)
		}
		for _, entry := range service.NeedsRecreate {
			add(entry)
		}
		for _, entry := range service.Create {
			add(entry)
		}
	}

	names := make([]string, 0, len(byName))
	for name := range byName {
		names = append(names, name)
	}
	sort.Strings(names)

	out := make([]ContainerResult, 0, len(names))
	for _, name := range names {
		item := byName[name]
		out = append(out, ContainerResult{
			MachineID:     item.machineID,
			ContainerName: item.name,
			Expected:      item.image,
		})
	}
	return out
}
