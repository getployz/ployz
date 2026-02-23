package client

import (
	"context"
	"errors"
	"io"
	"time"

	"ployz/internal/daemon/pb"
	"ployz/pkg/sdk/types"
)

func (c *Client) PlanDeploy(ctx context.Context, network, namespace string, composeSpec []byte) (types.DeployPlan, error) {
	resp, err := c.daemon.PlanDeploy(ctx, &pb.PlanDeployRequest{
		Network:     network,
		Namespace:   namespace,
		ComposeSpec: composeSpec,
	})
	if err != nil {
		return types.DeployPlan{}, grpcErr(err)
	}
	return deployPlanFromProto(resp.Plan), nil
}

func (c *Client) ApplyDeploy(ctx context.Context, network, namespace string, composeSpec []byte, events chan<- types.DeployProgressEvent) (types.DeployResult, error) {
	stream, err := c.daemon.ApplyDeploy(ctx, &pb.ApplyDeployRequest{
		Network:     network,
		Namespace:   namespace,
		ComposeSpec: composeSpec,
	})
	if err != nil {
		return types.DeployResult{}, grpcErr(err)
	}

	for {
		msg, recvErr := stream.Recv()
		if errors.Is(recvErr, io.EOF) {
			return types.DeployResult{}, errors.New("apply deploy stream closed without result")
		}
		if recvErr != nil {
			return types.DeployResult{}, grpcErr(recvErr)
		}

		if msg.Progress != nil {
			event := progressEventFromProto(msg.Progress)
			if events != nil {
				select {
				case events <- event:
				default:
				}
			}
		}

		if msg.Result != nil {
			return deployResultFromProto(msg.Result), nil
		}
	}
}

func (c *Client) ListDeployments(ctx context.Context, network, namespace string) ([]types.DeploymentEntry, error) {
	resp, err := c.daemon.ListDeployments(ctx, &pb.ListDeploymentsRequest{
		Network:   network,
		Namespace: namespace,
	})
	if err != nil {
		return nil, grpcErr(err)
	}

	out := make([]types.DeploymentEntry, 0, len(resp.Deployments))
	for _, row := range resp.Deployments {
		out = append(out, deploymentEntryFromProto(row))
	}
	return out, nil
}

func (c *Client) RemoveNamespace(ctx context.Context, network, namespace string) error {
	_, err := c.daemon.RemoveNamespace(ctx, &pb.RemoveNamespaceRequest{
		Network:   network,
		Namespace: namespace,
	})
	return grpcErr(err)
}

func (c *Client) ReadContainerState(ctx context.Context, network, namespace string) ([]types.ContainerState, error) {
	resp, err := c.daemon.ReadContainerState(ctx, &pb.ReadContainerStateRequest{
		Network:   network,
		Namespace: namespace,
	})
	if err != nil {
		return nil, grpcErr(err)
	}

	out := make([]types.ContainerState, 0, len(resp.Containers))
	for _, cstate := range resp.Containers {
		out = append(out, types.ContainerState{
			ContainerName: cstate.ContainerName,
			Image:         cstate.Image,
			Running:       cstate.Running,
			Healthy:       cstate.Healthy,
		})
	}
	return out, nil
}

func deployPlanFromProto(p *pb.DeployPlanProto) types.DeployPlan {
	if p == nil {
		return types.DeployPlan{}
	}
	out := types.DeployPlan{
		Namespace: p.Namespace,
		DeployID:  p.DeployId,
		Tiers:     make([]types.DeployTier, 0, len(p.Tiers)),
	}
	for _, tier := range p.Tiers {
		services := make([]types.DeployServicePlan, 0, len(tier.Services))
		for _, svc := range tier.Services {
			services = append(services, deployServicePlanFromProto(svc))
		}
		out.Tiers = append(out.Tiers, types.DeployTier{Services: services})
	}
	return out
}

func deployServicePlanFromProto(p *pb.ServicePlanProto) types.DeployServicePlan {
	if p == nil {
		return types.DeployServicePlan{}
	}
	return types.DeployServicePlan{
		Name:            p.Name,
		UpToDate:        deployEntriesFromProto(p.UpToDate),
		NeedsSpecUpdate: deployEntriesFromProto(p.NeedsSpecUpdate),
		NeedsUpdate:     deployEntriesFromProto(p.NeedsUpdate),
		NeedsRecreate:   deployEntriesFromProto(p.NeedsRecreate),
		Create:          deployEntriesFromProto(p.Create),
		Remove:          deployEntriesFromProto(p.Remove),
		UpdateConfig: types.DeployUpdateConfig{
			Order:         p.UpdateConfig.GetOrder(),
			Parallelism:   int(p.UpdateConfig.GetParallelism()),
			FailureAction: p.UpdateConfig.GetFailureAction(),
		},
		HealthCheck: deployHealthCheckFromProto(p.HealthCheck),
	}
}

func deployEntriesFromProto(entries []*pb.PlanEntryProto) []types.DeployPlanEntry {
	out := make([]types.DeployPlanEntry, 0, len(entries))
	for _, entry := range entries {
		out = append(out, types.DeployPlanEntry{
			MachineID:      entry.GetMachineId(),
			ContainerName:  entry.GetContainerName(),
			SpecJSON:       entry.GetSpecJson(),
			CurrentRowJSON: entry.GetCurrentRowJson(),
			Reason:         entry.GetReason(),
		})
	}
	return out
}

func deployHealthCheckFromProto(h *pb.HealthCheckProto) *types.DeployHealthCheck {
	if h == nil {
		return nil
	}
	return &types.DeployHealthCheck{
		Test:        append([]string(nil), h.Test...),
		Interval:    time.Duration(h.IntervalNs),
		Timeout:     time.Duration(h.TimeoutNs),
		Retries:     int(h.Retries),
		StartPeriod: time.Duration(h.StartPeriodNs),
	}
}

func progressEventFromProto(p *pb.ProgressEventProto) types.DeployProgressEvent {
	if p == nil {
		return types.DeployProgressEvent{}
	}
	return types.DeployProgressEvent{
		Type:      p.Type,
		Tier:      int(p.Tier),
		Service:   p.Service,
		MachineID: p.MachineId,
		Container: p.Container,
		Message:   p.Message,
	}
}

func deployResultFromProto(r *pb.DeployResultProto) types.DeployResult {
	if r == nil {
		return types.DeployResult{}
	}
	out := types.DeployResult{
		Namespace:    r.Namespace,
		DeployID:     r.DeployId,
		Status:       r.Status,
		ErrorMessage: r.ErrorMessage,
		ErrorPhase:   r.ErrorPhase,
		ErrorTier:    int(r.ErrorTier),
		Tiers:        make([]types.DeployTierResult, 0, len(r.Tiers)),
	}
	for _, tier := range r.Tiers {
		containers := make([]types.DeployContainerResult, 0, len(tier.Containers))
		for _, container := range tier.Containers {
			containers = append(containers, types.DeployContainerResult{
				MachineID:     container.MachineId,
				ContainerName: container.ContainerName,
				Expected:      container.Expected,
				Actual:        container.Actual,
				Match:         container.Match,
			})
		}
		out.Tiers = append(out.Tiers, types.DeployTierResult{
			Name:       tier.Name,
			Status:     tier.Status,
			Containers: containers,
		})
	}
	return out
}

func deploymentEntryFromProto(row *pb.DeploymentEntryProto) types.DeploymentEntry {
	if row == nil {
		return types.DeploymentEntry{}
	}
	labels := make(map[string]string, len(row.Labels))
	for key, value := range row.Labels {
		labels[key] = value
	}
	machineIDs := append([]string(nil), row.MachineIds...)
	return types.DeploymentEntry{
		ID:             row.Id,
		Namespace:      row.Namespace,
		Status:         row.Status,
		Owner:          row.Owner,
		OwnerHeartbeat: row.OwnerHeartbeat,
		Labels:         labels,
		MachineIDs:     machineIDs,
		Version:        row.Version,
		CreatedAt:      row.CreatedAt,
		UpdatedAt:      row.UpdatedAt,
	}
}
