package api

import (
	"context"
	"errors"

	pb "ployz/internal/controlplane/pb"
	"ployz/internal/deploy"
	"ployz/pkg/sdk/types"

	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

const (
	deployStreamEventBuffer = 256
)

func (s *Server) PlanDeploy(ctx context.Context, req *pb.PlanDeployRequest) (*pb.PlanDeployResponse, error) {
	if req == nil {
		return nil, status.Error(codes.InvalidArgument, "request is required")
	}
	if req.Namespace == "" || len(req.ComposeSpec) == 0 {
		return nil, status.Error(codes.InvalidArgument, "namespace and compose_spec are required")
	}

	plan, err := s.manager.PlanDeploy(ctx, req.Namespace, req.ComposeSpec)
	if err != nil {
		return nil, deployErrToStatus(err)
	}

	return &pb.PlanDeployResponse{Plan: deployPlanToProto(plan)}, nil
}

func (s *Server) ApplyDeploy(req *pb.ApplyDeployRequest, stream pb.Daemon_ApplyDeployServer) error {
	if req == nil {
		return status.Error(codes.InvalidArgument, "request is required")
	}
	if req.Namespace == "" || len(req.ComposeSpec) == 0 {
		return status.Error(codes.InvalidArgument, "namespace and compose_spec are required")
	}

	events := make(chan types.DeployProgressEvent, deployStreamEventBuffer)
	done := make(chan struct{})

	var result types.DeployResult
	var applyErr error
	go func() {
		result, applyErr = s.manager.ApplyDeploy(stream.Context(), req.Namespace, req.ComposeSpec, events)
		close(events)
		close(done)
	}()

	for {
		select {
		case <-stream.Context().Done():
			return status.Error(codes.Canceled, stream.Context().Err().Error())
		case ev, ok := <-events:
			if !ok {
				<-done
				if applyErr != nil {
					if result.ErrorMessage == "" {
						result.ErrorMessage = applyErr.Error()
					}
					if result.Status == "" {
						result.Status = deploy.DeployFailed.String()
					}

					var deployErr *deploy.DeployError
					if errors.As(applyErr, &deployErr) {
						if result.ErrorPhase == "" {
							result.ErrorPhase = deployErr.Phase.String()
						}
						if result.ErrorTier == 0 {
							result.ErrorTier = deployErr.Tier
						}
					}
				}
				if result.Status == "" {
					result.Status = deploy.DeploySucceeded.String()
				}
				return stream.Send(&pb.DeployProgressEvent{Result: deployResultToProto(result)})
			}

			if err := stream.Send(&pb.DeployProgressEvent{Progress: deployProgressEventToProto(ev)}); err != nil {
				return err
			}
		}
	}
}

func (s *Server) ListDeployments(ctx context.Context, req *pb.ListDeploymentsRequest) (*pb.ListDeploymentsResponse, error) {
	if req == nil {
		return nil, status.Error(codes.InvalidArgument, "request is required")
	}
	if req.Namespace == "" {
		return nil, status.Error(codes.InvalidArgument, "namespace is required")
	}

	rows, err := s.manager.ListDeployments(ctx, req.Namespace)
	if err != nil {
		return nil, deployErrToStatus(err)
	}

	out := make([]*pb.DeploymentEntryProto, 0, len(rows))
	for _, row := range rows {
		out = append(out, deploymentEntryToProto(row))
	}
	return &pb.ListDeploymentsResponse{Deployments: out}, nil
}

func (s *Server) RemoveNamespace(ctx context.Context, req *pb.RemoveNamespaceRequest) (*pb.RemoveNamespaceResponse, error) {
	if req == nil {
		return nil, status.Error(codes.InvalidArgument, "request is required")
	}
	if req.Namespace == "" {
		return nil, status.Error(codes.InvalidArgument, "namespace is required")
	}

	if err := s.manager.RemoveNamespace(ctx, req.Namespace); err != nil {
		return nil, deployErrToStatus(err)
	}
	return &pb.RemoveNamespaceResponse{}, nil
}

func (s *Server) ReadContainerState(ctx context.Context, req *pb.ReadContainerStateRequest) (*pb.ReadContainerStateResponse, error) {
	if req == nil {
		return nil, status.Error(codes.InvalidArgument, "request is required")
	}
	if req.Namespace == "" {
		return nil, status.Error(codes.InvalidArgument, "namespace is required")
	}

	rows, err := s.manager.ReadContainerState(ctx, req.Namespace)
	if err != nil {
		return nil, deployErrToStatus(err)
	}

	out := make([]*pb.ContainerStateProto, 0, len(rows))
	for _, row := range rows {
		out = append(out, &pb.ContainerStateProto{
			ContainerName: row.ContainerName,
			Image:         row.Image,
			Running:       row.Running,
			Healthy:       row.Healthy,
		})
	}
	return &pb.ReadContainerStateResponse{Containers: out}, nil
}

func deployErrToStatus(err error) error {
	if err == nil {
		return nil
	}

	var deployErr *deploy.DeployError
	if errors.As(err, &deployErr) {
		message := deployErr.Message
		if message == "" {
			message = deployErr.Error()
		}
		switch deployErr.Phase {
		case deploy.DeployErrorPhaseOwnership:
			return status.Error(codes.Aborted, message)
		case deploy.DeployErrorPhasePrePull:
			return status.Error(codes.Unavailable, message)
		case deploy.DeployErrorPhaseHealth:
			return status.Error(codes.FailedPrecondition, message)
		case deploy.DeployErrorPhasePostcondition:
			return status.Error(codes.Aborted, message)
		default:
			return status.Error(codes.Internal, message)
		}
	}

	return toGRPCError(err)
}

func deployPlanToProto(plan types.DeployPlan) *pb.DeployPlanProto {
	out := &pb.DeployPlanProto{
		Namespace: plan.Namespace,
		DeployId:  plan.DeployID,
		Tiers:     make([]*pb.TierProto, 0, len(plan.Tiers)),
	}
	for _, tier := range plan.Tiers {
		services := make([]*pb.ServicePlanProto, 0, len(tier.Services))
		for _, service := range tier.Services {
			services = append(services, deployServicePlanToProto(service))
		}
		out.Tiers = append(out.Tiers, &pb.TierProto{Services: services})
	}
	return out
}

func deployServicePlanToProto(service types.DeployServicePlan) *pb.ServicePlanProto {
	out := &pb.ServicePlanProto{
		Name:            service.Name,
		UpToDate:        deployPlanEntriesToProto(service.UpToDate),
		NeedsSpecUpdate: deployPlanEntriesToProto(service.NeedsSpecUpdate),
		NeedsUpdate:     deployPlanEntriesToProto(service.NeedsUpdate),
		NeedsRecreate:   deployPlanEntriesToProto(service.NeedsRecreate),
		Create:          deployPlanEntriesToProto(service.Create),
		Remove:          deployPlanEntriesToProto(service.Remove),
		UpdateConfig: &pb.UpdateConfigProto{
			Order:         service.UpdateConfig.Order,
			Parallelism:   int32(service.UpdateConfig.Parallelism),
			FailureAction: service.UpdateConfig.FailureAction,
		},
	}
	if service.HealthCheck != nil {
		out.HealthCheck = &pb.HealthCheckProto{
			Test:          append([]string(nil), service.HealthCheck.Test...),
			IntervalNs:    int64(service.HealthCheck.Interval),
			TimeoutNs:     int64(service.HealthCheck.Timeout),
			Retries:       int32(service.HealthCheck.Retries),
			StartPeriodNs: int64(service.HealthCheck.StartPeriod),
		}
	}
	return out
}

func deployPlanEntriesToProto(entries []types.DeployPlanEntry) []*pb.PlanEntryProto {
	out := make([]*pb.PlanEntryProto, 0, len(entries))
	for _, entry := range entries {
		out = append(out, &pb.PlanEntryProto{
			MachineId:      entry.MachineID,
			ContainerName:  entry.ContainerName,
			SpecJson:       entry.SpecJSON,
			CurrentRowJson: entry.CurrentRowJSON,
			Reason:         entry.Reason,
		})
	}
	return out
}

func deployProgressEventToProto(ev types.DeployProgressEvent) *pb.ProgressEventProto {
	return &pb.ProgressEventProto{
		Type:      ev.Type,
		Tier:      int32(ev.Tier),
		Service:   ev.Service,
		MachineId: ev.MachineID,
		Container: ev.Container,
		Message:   ev.Message,
	}
}

func deployResultToProto(result types.DeployResult) *pb.DeployResultProto {
	out := &pb.DeployResultProto{
		Namespace:    result.Namespace,
		DeployId:     result.DeployID,
		Status:       result.Status,
		ErrorMessage: result.ErrorMessage,
		ErrorPhase:   result.ErrorPhase,
		ErrorTier:    int32(result.ErrorTier),
		Tiers:        make([]*pb.TierResultProto, 0, len(result.Tiers)),
	}
	for _, tier := range result.Tiers {
		containers := make([]*pb.ContainerResultProto, 0, len(tier.Containers))
		for _, container := range tier.Containers {
			containers = append(containers, &pb.ContainerResultProto{
				MachineId:     container.MachineID,
				ContainerName: container.ContainerName,
				Expected:      container.Expected,
				Actual:        container.Actual,
				Match:         container.Match,
			})
		}
		out.Tiers = append(out.Tiers, &pb.TierResultProto{
			Name:       tier.Name,
			Status:     tier.Status,
			Containers: containers,
		})
	}
	return out
}

func deploymentEntryToProto(row types.DeploymentEntry) *pb.DeploymentEntryProto {
	labels := make(map[string]string, len(row.Labels))
	for key, value := range row.Labels {
		labels[key] = value
	}
	machineIDs := append([]string(nil), row.MachineIDs...)
	return &pb.DeploymentEntryProto{
		Id:             row.ID,
		Namespace:      row.Namespace,
		Status:         row.Status,
		Owner:          row.Owner,
		OwnerHeartbeat: row.OwnerHeartbeat,
		Labels:         labels,
		MachineIds:     machineIDs,
		Version:        row.Version,
		CreatedAt:      row.CreatedAt,
		UpdatedAt:      row.UpdatedAt,
	}
}
