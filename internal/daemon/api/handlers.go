package api

import (
	"context"

	pb "ployz/internal/daemon/pb"

	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

// --- gRPC methods: proto <-> types conversion boundary ---

func (s *Server) ApplyNetworkSpec(ctx context.Context, req *pb.ApplyNetworkSpecRequest) (*pb.ApplyResult, error) {
	if req.Spec == nil {
		return nil, status.Error(codes.InvalidArgument, "spec is required")
	}
	out, err := s.manager.ApplyNetworkSpec(ctx, specFromProto(req.Spec))
	if err != nil {
		return nil, toGRPCError(err)
	}
	return applyResultToProto(out), nil
}

func (s *Server) DisableNetwork(ctx context.Context, req *pb.DisableNetworkRequest) (*pb.DisableNetworkResponse, error) {
	if err := s.manager.DisableNetwork(ctx, req.Purge); err != nil {
		return nil, toGRPCError(err)
	}
	return &pb.DisableNetworkResponse{}, nil
}

func (s *Server) GetStatus(ctx context.Context, req *pb.GetStatusRequest) (*pb.NetworkStatus, error) {
	st, err := s.manager.GetStatus(ctx)
	if err != nil {
		return nil, toGRPCError(err)
	}
	return statusToProto(st), nil
}

func (s *Server) GetIdentity(ctx context.Context, req *pb.GetIdentityRequest) (*pb.Identity, error) {
	id, err := s.manager.GetIdentity(ctx)
	if err != nil {
		return nil, toGRPCError(err)
	}
	return identityToProto(id), nil
}

func (s *Server) ListMachines(ctx context.Context, req *pb.ListMachinesRequest) (*pb.ListMachinesResponse, error) {
	machines, err := s.manager.ListMachines(ctx)
	if err != nil {
		return nil, toGRPCError(err)
	}
	pbMachines := make([]*pb.MachineEntry, len(machines))
	for i, m := range machines {
		pbMachines[i] = machineEntryToProto(m)
	}
	return &pb.ListMachinesResponse{Machines: pbMachines}, nil
}

func (s *Server) UpsertMachine(ctx context.Context, req *pb.UpsertMachineRequest) (*pb.UpsertMachineResponse, error) {
	if req.Machine == nil {
		return nil, status.Error(codes.InvalidArgument, "machine is required")
	}
	if err := s.manager.UpsertMachine(ctx, machineEntryFromProto(req.Machine)); err != nil {
		return nil, toGRPCError(err)
	}
	return &pb.UpsertMachineResponse{}, nil
}

func (s *Server) RemoveMachine(ctx context.Context, req *pb.RemoveMachineRequest) (*pb.RemoveMachineResponse, error) {
	if err := s.manager.RemoveMachine(ctx, req.IdOrEndpoint); err != nil {
		return nil, toGRPCError(err)
	}
	return &pb.RemoveMachineResponse{}, nil
}

func (s *Server) GetPeerHealth(ctx context.Context, req *pb.GetPeerHealthRequest) (*pb.GetPeerHealthResponse, error) {
	responses, err := s.manager.GetPeerHealth(ctx)
	if err != nil {
		return nil, toGRPCError(err)
	}
	return peerHealthToProto(responses), nil
}
