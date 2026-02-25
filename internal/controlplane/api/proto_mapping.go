package api

import (
	pb "ployz/internal/controlplane/pb"
	"ployz/pkg/sdk/types"
)

// --- proto <-> types conversion helpers ---

func specFromProto(p *pb.NetworkSpec) types.NetworkSpec {
	return types.NetworkSpec{
		Network:           p.Network,
		DataRoot:          p.DataRoot,
		NetworkCIDR:       p.NetworkCidr,
		Subnet:            p.Subnet,
		ManagementIP:      p.ManagementIp,
		AdvertiseEndpoint: p.AdvertiseEndpoint,
		WGPort:            int(p.WgPort),
		CorrosionMemberID: p.CorrosionMemberId,
		CorrosionAPIToken: p.CorrosionApiToken,
		Bootstrap:         p.Bootstrap,
		HelperImage:       p.HelperImage,
	}
}

func applyResultToProto(r types.ApplyResult) *pb.ApplyResult {
	return &pb.ApplyResult{
		Network:             r.Network,
		NetworkCidr:         r.NetworkCIDR,
		Subnet:              r.Subnet,
		ManagementIp:        r.ManagementIP,
		WgInterface:         r.WGInterface,
		WgPort:              int32(r.WGPort),
		AdvertiseEndpoint:   r.AdvertiseEndpoint,
		CorrosionName:       r.CorrosionName,
		CorrosionApiAddr:    r.CorrosionAPIAddr,
		CorrosionGossipAddr: r.CorrosionGossipAddrPort,
		DockerNetwork:       r.DockerNetwork,
		SupervisorRunning:   r.SupervisorRunning,
	}
}

func statusToProto(st types.NetworkStatus) *pb.NetworkStatus {
	return &pb.NetworkStatus{
		Configured:        st.Configured,
		Running:           st.Running,
		Wireguard:         st.WireGuard,
		Corrosion:         st.Corrosion,
		Docker:            st.DockerNet,
		StatePath:         st.StatePath,
		SupervisorRunning: st.SupervisorRunning,
		NetworkPhase:      st.NetworkPhase,
		SupervisorPhase:   st.SupervisorPhase,
		SupervisorError:   st.SupervisorError,
		ClockPhase:        st.ClockPhase,
		DockerRequired:    st.DockerRequired,
		RuntimeTree:       stateNodeToProto(st.RuntimeTree),
		ClockHealth: &pb.ClockHealth{
			NtpOffsetMs: st.ClockHealth.NTPOffsetMs,
			NtpHealthy:  st.ClockHealth.NTPHealthy,
			NtpError:    st.ClockHealth.NTPError,
		},
	}
}

func stateNodeToProto(node types.StateNode) *pb.StateNode {
	out := &pb.StateNode{
		Component:     node.Component,
		Phase:         node.Phase,
		Required:      node.Required,
		Healthy:       node.Healthy,
		LastErrorCode: node.LastErrorCode,
		LastError:     node.LastError,
		Hint:          node.Hint,
	}
	if len(node.Children) == 0 {
		return out
	}
	out.Children = make([]*pb.StateNode, len(node.Children))
	for i, child := range node.Children {
		out.Children[i] = stateNodeToProto(child)
	}
	return out
}

func identityToProto(id types.Identity) *pb.Identity {
	return &pb.Identity{
		Id:                  id.ID,
		PublicKey:           id.PublicKey,
		Subnet:              id.Subnet,
		ManagementIp:        id.ManagementIP,
		AdvertiseEndpoint:   id.AdvertiseEndpoint,
		NetworkCidr:         id.NetworkCIDR,
		WgInterface:         id.WGInterface,
		WgPort:              int32(id.WGPort),
		HelperName:          id.HelperName,
		CorrosionGossipPort: int32(id.CorrosionGossipPort),
		CorrosionMemberId:   id.CorrosionMemberID,
		CorrosionApiToken:   id.CorrosionAPIToken,
		Running:             id.Running,
	}
}

func machineEntryToProto(m types.MachineEntry) *pb.MachineEntry {
	return &pb.MachineEntry{
		Id:               m.ID,
		PublicKey:        m.PublicKey,
		Subnet:           m.Subnet,
		ManagementIp:     m.ManagementIP,
		Endpoint:         m.Endpoint,
		LastUpdated:      m.LastUpdated,
		Version:          m.Version,
		ExpectedVersion:  m.ExpectedVersion,
		FreshnessMs:      float64(m.Freshness.Milliseconds()),
		Stale:            m.Stale,
		ReplicationLagMs: float64(m.ReplicationLag.Milliseconds()),
	}
}

func machineEntryFromProto(p *pb.MachineEntry) types.MachineEntry {
	return types.MachineEntry{
		ID:              p.Id,
		PublicKey:       p.PublicKey,
		Subnet:          p.Subnet,
		ManagementIP:    p.ManagementIp,
		Endpoint:        p.Endpoint,
		LastUpdated:     p.LastUpdated,
		Version:         p.Version,
		ExpectedVersion: p.ExpectedVersion,
	}
}

func peerHealthToProto(responses []types.PeerHealthResponse) *pb.GetPeerHealthResponse {
	messages := make([]*pb.PeerHealthReply, len(responses))
	for i, r := range responses {
		peers := make([]*pb.PeerLag, len(r.Peers))
		for j, p := range r.Peers {
			var pingMs float64
			switch {
			case p.PingRTT < 0:
				pingMs = -1
			case p.PingRTT > 0:
				pingMs = float64(p.PingRTT.Microseconds()) / 1000.0
			}
			peers[j] = &pb.PeerLag{
				NodeId:           p.NodeID,
				FreshnessMs:      float64(p.Freshness.Milliseconds()),
				Stale:            p.Stale,
				ReplicationLagMs: float64(p.ReplicationLag.Milliseconds()),
				PingMs:           pingMs,
			}
		}
		msg := &pb.PeerHealthReply{
			NodeId: r.NodeID,
			Ntp: &pb.ClockHealth{
				NtpOffsetMs: r.NTP.NTPOffsetMs,
				NtpHealthy:  r.NTP.NTPHealthy,
				NtpError:    r.NTP.NTPError,
			},
			Peers: peers,
		}
		if r.MachineAddr != "" || r.MachineID != "" || r.Error != "" {
			msg.Metadata = &pb.Metadata{
				MachineAddr: r.MachineAddr,
				MachineId:   r.MachineID,
				Error:       r.Error,
			}
		}
		messages[i] = msg
	}
	return &pb.GetPeerHealthResponse{Messages: messages}
}
