package machine

import (
	"context"
	"errors"
	"net/netip"
	"testing"

	"ployz/pkg/sdk/types"
)

func TestConfigureOmitsCorrosionIdentity(t *testing.T) {
	t.Parallel()

	networkCIDR := netip.MustParsePrefix("10.210.0.0/16")
	remoteSubnet := netip.MustParsePrefix("10.210.1.0/24")

	var seen types.NetworkSpec
	api := &stubAPI{
		applyNetworkSpec: func(_ context.Context, spec types.NetworkSpec) (types.ApplyResult, error) {
			seen = spec
			if spec.CorrosionMemberID != 0 {
				return types.ApplyResult{}, errors.New("unexpected corrosion member id override")
			}
			if spec.CorrosionAPIToken != "" {
				return types.ApplyResult{}, errors.New("unexpected corrosion api token override")
			}
			return types.ApplyResult{}, nil
		},
		getIdentity: func(context.Context) (types.Identity, error) {
			return types.Identity{
				ID:                "remote-node",
				PublicKey:         "remote-node",
				Subnet:            remoteSubnet.String(),
				ManagementIP:      "fd8c:88ad:7f06:fb52:5d00:5b43:dcb9:4fe2",
				AdvertiseEndpoint: "5.223.76.220:51820",
			}, nil
		},
	}

	op := &addOp{
		network:      "default",
		remoteRoot:   remoteLinuxDataRoot,
		networkCIDR:  networkCIDR,
		remoteSubnet: remoteSubnet,
		remoteEP:     "5.223.76.220:51820",
		bootstrap:    []string{"fd8c:88ad:7f06:e9a2:61e3:ed84:eae3:69bb:53094"},
		opts: AddOptions{
			WGPort: 51820,
		},
		remoteAPI: api,
	}

	if err := op.configure(context.Background()); err != nil {
		t.Fatalf("configure(): %v", err)
	}

	if seen.CorrosionMemberID != 0 {
		t.Fatalf("configure() sent corrosion_member_id=%d, want 0", seen.CorrosionMemberID)
	}
	if seen.CorrosionAPIToken != "" {
		t.Fatalf("configure() sent corrosion_api_token=%q, want empty", seen.CorrosionAPIToken)
	}
	if op.entry.ID != "remote-node" {
		t.Fatalf("configure() entry.ID = %q, want remote-node", op.entry.ID)
	}
}

func TestConfigureReturnsApplyError(t *testing.T) {
	t.Parallel()

	wantErr := errors.New("apply failed")
	api := &stubAPI{
		applyNetworkSpec: func(context.Context, types.NetworkSpec) (types.ApplyResult, error) {
			return types.ApplyResult{}, wantErr
		},
	}

	op := &addOp{
		network:      "default",
		remoteRoot:   remoteLinuxDataRoot,
		networkCIDR:  netip.MustParsePrefix("10.210.0.0/16"),
		remoteSubnet: netip.MustParsePrefix("10.210.1.0/24"),
		remoteEP:     "5.223.76.220:51820",
		opts: AddOptions{
			WGPort: 51820,
		},
		remoteAPI: api,
	}

	err := op.configure(context.Background())
	if !errors.Is(err, wantErr) {
		t.Fatalf("configure() error = %v, want %v", err, wantErr)
	}
}

type stubAPI struct {
	applyNetworkSpec func(context.Context, types.NetworkSpec) (types.ApplyResult, error)
	getIdentity      func(context.Context) (types.Identity, error)
}

func (s *stubAPI) ApplyNetworkSpec(ctx context.Context, spec types.NetworkSpec) (types.ApplyResult, error) {
	if s.applyNetworkSpec != nil {
		return s.applyNetworkSpec(ctx, spec)
	}
	return types.ApplyResult{}, nil
}

func (s *stubAPI) DisableNetwork(context.Context, bool) error { return nil }

func (s *stubAPI) GetStatus(context.Context) (types.NetworkStatus, error) {
	return types.NetworkStatus{}, nil
}

func (s *stubAPI) GetIdentity(ctx context.Context) (types.Identity, error) {
	if s.getIdentity != nil {
		return s.getIdentity(ctx)
	}
	return types.Identity{}, nil
}

func (s *stubAPI) ListMachines(context.Context) ([]types.MachineEntry, error) { return nil, nil }

func (s *stubAPI) UpsertMachine(context.Context, types.MachineEntry) error { return nil }

func (s *stubAPI) RemoveMachine(context.Context, string) error { return nil }

func (s *stubAPI) GetPeerHealth(context.Context) ([]types.PeerHealthResponse, error) { return nil, nil }

func (s *stubAPI) PlanDeploy(context.Context, string, []byte) (types.DeployPlan, error) {
	return types.DeployPlan{}, nil
}

func (s *stubAPI) ApplyDeploy(context.Context, string, []byte, chan<- types.DeployProgressEvent) (types.DeployResult, error) {
	return types.DeployResult{}, nil
}

func (s *stubAPI) ListDeployments(context.Context, string) ([]types.DeploymentEntry, error) {
	return nil, nil
}

func (s *stubAPI) RemoveNamespace(context.Context, string) error { return nil }

func (s *stubAPI) ReadContainerState(context.Context, string) ([]types.ContainerState, error) {
	return nil, nil
}
