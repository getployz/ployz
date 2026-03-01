package api

import (
	"context"

	pb "ployz/internal/daemon/pb"

	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

const deployUnimplementedMessage = "deploy is being rebuilt - not yet available"

func (s *Server) PlanDeploy(ctx context.Context, req *pb.PlanDeployRequest) (*pb.PlanDeployResponse, error) {
	return nil, status.Error(codes.Unimplemented, deployUnimplementedMessage)
}

func (s *Server) ApplyDeploy(req *pb.ApplyDeployRequest, stream pb.Daemon_ApplyDeployServer) error {
	return status.Error(codes.Unimplemented, deployUnimplementedMessage)
}

func (s *Server) ListDeployments(ctx context.Context, req *pb.ListDeploymentsRequest) (*pb.ListDeploymentsResponse, error) {
	return nil, status.Error(codes.Unimplemented, deployUnimplementedMessage)
}

func (s *Server) RemoveNamespace(ctx context.Context, req *pb.RemoveNamespaceRequest) (*pb.RemoveNamespaceResponse, error) {
	return nil, status.Error(codes.Unimplemented, deployUnimplementedMessage)
}

func (s *Server) ReadContainerState(ctx context.Context, req *pb.ReadContainerStateRequest) (*pb.ReadContainerStateResponse, error) {
	return nil, status.Error(codes.Unimplemented, deployUnimplementedMessage)
}
