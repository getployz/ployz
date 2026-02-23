package server

import (
	"errors"
	"fmt"
	"os"
	"testing"
	"time"

	pb "ployz/internal/daemon/pb"
	"ployz/internal/deploy"
	"ployz/internal/mesh"
	"ployz/pkg/sdk/types"

	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

func TestToGRPCError(t *testing.T) {
	tests := []struct {
		name     string
		err      error
		wantNil  bool
		wantCode codes.Code
	}{
		{
			name:    "nil error",
			err:     nil,
			wantNil: true,
		},
		{
			name:     "ErrConflict",
			err:      mesh.ErrConflict,
			wantCode: codes.FailedPrecondition,
		},
		{
			name:     "wrapped ErrConflict",
			err:      fmt.Errorf("upsert machine: %w", mesh.ErrConflict),
			wantCode: codes.FailedPrecondition,
		},
		{
			name:     "os.ErrNotExist",
			err:      os.ErrNotExist,
			wantCode: codes.NotFound,
		},
		{
			name:     "wrapped os.ErrNotExist",
			err:      fmt.Errorf("load state: %w", os.ErrNotExist),
			wantCode: codes.NotFound,
		},
		{
			name:     "ErrNotInitialized",
			err:      mesh.ErrNotInitialized,
			wantCode: codes.NotFound,
		},
		{
			name:     "wrapped ErrNotInitialized",
			err:      fmt.Errorf("network default: %w", mesh.ErrNotInitialized),
			wantCode: codes.NotFound,
		},
		{
			name:     "ValidationError",
			err:      &mesh.ValidationError{Field: "subnet", Message: "is required"},
			wantCode: codes.InvalidArgument,
		},
		{
			name:     "wrapped ValidationError",
			err:      fmt.Errorf("upsert machine: %w", &mesh.ValidationError{Field: "public_key", Message: "invalid format"}),
			wantCode: codes.InvalidArgument,
		},
		{
			name:     "is not initialized (string fallback)",
			err:      errors.New("network default is not initialized"),
			wantCode: codes.NotFound,
		},
		{
			name:     "is required",
			err:      errors.New("network is required"),
			wantCode: codes.InvalidArgument,
		},
		{
			name:     "must be",
			err:      errors.New("port must be between 1 and 65535"),
			wantCode: codes.InvalidArgument,
		},
		{
			name:     "parse error",
			err:      errors.New("parse network cidr: invalid prefix"),
			wantCode: codes.InvalidArgument,
		},
		{
			name:     "connect to docker",
			err:      errors.New("cannot connect to docker daemon"),
			wantCode: codes.Unavailable,
		},
		{
			name:     "docker daemon",
			err:      errors.New("docker daemon is not running"),
			wantCode: codes.Unavailable,
		},
		{
			name:     "generic error",
			err:      errors.New("something unexpected happened"),
			wantCode: codes.Internal,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := toGRPCError(tt.err)
			if tt.wantNil {
				if result != nil {
					t.Fatalf("toGRPCError(nil) = %v, want nil", result)
				}
				return
			}
			if result == nil {
				t.Fatal("toGRPCError() = nil, want non-nil")
			}
			st, ok := status.FromError(result)
			if !ok {
				t.Fatalf("result is not a gRPC status error: %v", result)
			}
			if st.Code() != tt.wantCode {
				t.Errorf("code: got %v, want %v", st.Code(), tt.wantCode)
			}
		})
	}
}

func TestDeployErrToStatus(t *testing.T) {
	tests := []struct {
		name     string
		err      error
		wantCode codes.Code
	}{
		{
			name: "ownership phase",
			err: &deploy.DeployError{
				Phase:   "ownership",
				Message: "deploy ownership lost",
			},
			wantCode: codes.Aborted,
		},
		{
			name: "pre-pull phase",
			err: &deploy.DeployError{
				Phase:   "pre-pull",
				Message: "image pull failed",
			},
			wantCode: codes.Unavailable,
		},
		{
			name: "health phase",
			err: &deploy.DeployError{
				Phase:   "health",
				Message: "container unhealthy",
			},
			wantCode: codes.FailedPrecondition,
		},
		{
			name: "postcondition phase",
			err: &deploy.DeployError{
				Phase:   "postcondition",
				Message: "state mismatch",
			},
			wantCode: codes.Aborted,
		},
		{
			name:     "fallback",
			err:      errors.New("network is required"),
			wantCode: codes.InvalidArgument,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := deployErrToStatus(tt.err)
			st, ok := status.FromError(got)
			if !ok {
				t.Fatalf("deployErrToStatus() returned non-status error: %v", got)
			}
			if st.Code() != tt.wantCode {
				t.Fatalf("status code = %v, want %v", st.Code(), tt.wantCode)
			}
		})
	}
}

func TestSpecFromProto(t *testing.T) {
	t.Run("all fields populated", func(t *testing.T) {
		p := &pb.NetworkSpec{
			Network:           "test-net",
			DataRoot:          "/tmp/data",
			NetworkCidr:       "10.210.0.0/16",
			Subnet:            "10.210.1.0/24",
			ManagementIp:      "10.210.1.1",
			AdvertiseEndpoint: "5.9.85.203:51820",
			WgPort:            51820,
			CorrosionMemberId: 42,
			CorrosionApiToken: "secret-token",
			Bootstrap:         []string{"10.0.0.1:53094", "10.0.0.2:53094"},
			HelperImage:       "ghcr.io/test/helper:latest",
		}

		got := specFromProto(p)

		if got.Network != "test-net" {
			t.Errorf("Network: got %q, want %q", got.Network, "test-net")
		}
		if got.DataRoot != "/tmp/data" {
			t.Errorf("DataRoot: got %q, want %q", got.DataRoot, "/tmp/data")
		}
		if got.NetworkCIDR != "10.210.0.0/16" {
			t.Errorf("NetworkCIDR: got %q, want %q", got.NetworkCIDR, "10.210.0.0/16")
		}
		if got.Subnet != "10.210.1.0/24" {
			t.Errorf("Subnet: got %q, want %q", got.Subnet, "10.210.1.0/24")
		}
		if got.ManagementIP != "10.210.1.1" {
			t.Errorf("ManagementIP: got %q, want %q", got.ManagementIP, "10.210.1.1")
		}
		if got.AdvertiseEndpoint != "5.9.85.203:51820" {
			t.Errorf("AdvertiseEndpoint: got %q, want %q", got.AdvertiseEndpoint, "5.9.85.203:51820")
		}
		if got.WGPort != 51820 {
			t.Errorf("WGPort: got %d, want %d", got.WGPort, 51820)
		}
		if got.CorrosionMemberID != 42 {
			t.Errorf("CorrosionMemberID: got %d, want %d", got.CorrosionMemberID, 42)
		}
		if got.CorrosionAPIToken != "secret-token" {
			t.Errorf("CorrosionAPIToken: got %q, want %q", got.CorrosionAPIToken, "secret-token")
		}
		if len(got.Bootstrap) != 2 {
			t.Fatalf("Bootstrap len: got %d, want 2", len(got.Bootstrap))
		}
		if got.Bootstrap[0] != "10.0.0.1:53094" {
			t.Errorf("Bootstrap[0]: got %q, want %q", got.Bootstrap[0], "10.0.0.1:53094")
		}
		if got.Bootstrap[1] != "10.0.0.2:53094" {
			t.Errorf("Bootstrap[1]: got %q, want %q", got.Bootstrap[1], "10.0.0.2:53094")
		}
		if got.HelperImage != "ghcr.io/test/helper:latest" {
			t.Errorf("HelperImage: got %q, want %q", got.HelperImage, "ghcr.io/test/helper:latest")
		}
	})

	t.Run("partial fields", func(t *testing.T) {
		p := &pb.NetworkSpec{
			Network: "minimal",
			WgPort:  9999,
		}

		got := specFromProto(p)

		if got.Network != "minimal" {
			t.Errorf("Network: got %q, want %q", got.Network, "minimal")
		}
		if got.WGPort != 9999 {
			t.Errorf("WGPort: got %d, want %d", got.WGPort, 9999)
		}
		if got.DataRoot != "" {
			t.Errorf("DataRoot: got %q, want empty", got.DataRoot)
		}
		if got.NetworkCIDR != "" {
			t.Errorf("NetworkCIDR: got %q, want empty", got.NetworkCIDR)
		}
		if got.Subnet != "" {
			t.Errorf("Subnet: got %q, want empty", got.Subnet)
		}
		if got.ManagementIP != "" {
			t.Errorf("ManagementIP: got %q, want empty", got.ManagementIP)
		}
		if got.AdvertiseEndpoint != "" {
			t.Errorf("AdvertiseEndpoint: got %q, want empty", got.AdvertiseEndpoint)
		}
		if got.CorrosionMemberID != 0 {
			t.Errorf("CorrosionMemberID: got %d, want 0", got.CorrosionMemberID)
		}
		if got.CorrosionAPIToken != "" {
			t.Errorf("CorrosionAPIToken: got %q, want empty", got.CorrosionAPIToken)
		}
		if len(got.Bootstrap) != 0 {
			t.Errorf("Bootstrap: got %v, want empty", got.Bootstrap)
		}
		if got.HelperImage != "" {
			t.Errorf("HelperImage: got %q, want empty", got.HelperImage)
		}
	})
}

func TestPeerHealthToProto(t *testing.T) {
	t.Run("empty peers", func(t *testing.T) {
		got := peerHealthToProto(nil)
		if len(got.Messages) != 0 {
			t.Errorf("Messages len: got %d, want 0", len(got.Messages))
		}

		got = peerHealthToProto([]types.PeerHealthResponse{})
		if len(got.Messages) != 0 {
			t.Errorf("Messages len: got %d, want 0", len(got.Messages))
		}
	})

	t.Run("ping RTT conversions", func(t *testing.T) {
		responses := []types.PeerHealthResponse{
			{
				NodeID: "node-1",
				NTP: types.ClockHealth{
					NTPOffsetMs: 1.5,
					NTPHealthy:  true,
				},
				Peers: []types.PeerLag{
					{
						NodeID:  "peer-zero-rtt",
						PingRTT: 0,
					},
					{
						NodeID:  "peer-unreachable",
						PingRTT: -1,
					},
					{
						NodeID:  "peer-normal",
						PingRTT: 1500 * time.Microsecond, // 1.5ms
					},
				},
			},
		}

		got := peerHealthToProto(responses)

		if len(got.Messages) != 1 {
			t.Fatalf("Messages len: got %d, want 1", len(got.Messages))
		}
		msg := got.Messages[0]
		if msg.NodeId != "node-1" {
			t.Errorf("NodeId: got %q, want %q", msg.NodeId, "node-1")
		}
		if msg.Ntp.NtpOffsetMs != 1.5 {
			t.Errorf("NtpOffsetMs: got %f, want 1.5", msg.Ntp.NtpOffsetMs)
		}
		if !msg.Ntp.NtpHealthy {
			t.Error("NtpHealthy: got false, want true")
		}

		if len(msg.Peers) != 3 {
			t.Fatalf("Peers len: got %d, want 3", len(msg.Peers))
		}

		// PingRTT = 0 → 0ms
		if msg.Peers[0].PingMs != 0 {
			t.Errorf("PingMs for zero RTT: got %f, want 0", msg.Peers[0].PingMs)
		}

		// PingRTT = -1 → -1ms
		if msg.Peers[1].PingMs != -1 {
			t.Errorf("PingMs for unreachable: got %f, want -1", msg.Peers[1].PingMs)
		}

		// PingRTT = 1500us → 1.5ms (microsecond conversion)
		if msg.Peers[2].PingMs != 1.5 {
			t.Errorf("PingMs for normal: got %f, want 1.5", msg.Peers[2].PingMs)
		}
	})

	t.Run("metadata populated when present", func(t *testing.T) {
		responses := []types.PeerHealthResponse{
			{
				NodeID:      "node-1",
				MachineAddr: "10.0.0.1:54000",
				MachineID:   "machine-abc",
				Error:       "connection refused",
				NTP:         types.ClockHealth{},
				Peers:       nil,
			},
		}

		got := peerHealthToProto(responses)

		if len(got.Messages) != 1 {
			t.Fatalf("Messages len: got %d, want 1", len(got.Messages))
		}
		msg := got.Messages[0]
		if msg.Metadata == nil {
			t.Fatal("Metadata: got nil, want non-nil")
		}
		if msg.Metadata.MachineAddr != "10.0.0.1:54000" {
			t.Errorf("MachineAddr: got %q, want %q", msg.Metadata.MachineAddr, "10.0.0.1:54000")
		}
		if msg.Metadata.MachineId != "machine-abc" {
			t.Errorf("MachineId: got %q, want %q", msg.Metadata.MachineId, "machine-abc")
		}
		if msg.Metadata.Error != "connection refused" {
			t.Errorf("Error: got %q, want %q", msg.Metadata.Error, "connection refused")
		}
	})

	t.Run("no metadata when all fields empty", func(t *testing.T) {
		responses := []types.PeerHealthResponse{
			{
				NodeID: "node-1",
				NTP:    types.ClockHealth{},
				Peers:  nil,
			},
		}

		got := peerHealthToProto(responses)

		if len(got.Messages) != 1 {
			t.Fatalf("Messages len: got %d, want 1", len(got.Messages))
		}
		if got.Messages[0].Metadata != nil {
			t.Errorf("Metadata: got %v, want nil", got.Messages[0].Metadata)
		}
	})
}
