package types

import "time"

type NetworkSpec struct {
	Network           string   `json:"network"`
	DataRoot          string   `json:"data_root,omitempty"`
	NetworkCIDR       string   `json:"network_cidr,omitempty"`
	Subnet            string   `json:"subnet,omitempty"`
	ManagementIP      string   `json:"management_ip,omitempty"`
	AdvertiseEndpoint string   `json:"advertise_endpoint,omitempty"`
	WGPort            int      `json:"wg_port,omitempty"`
	CorrosionMemberID uint64   `json:"corrosion_member_id,omitempty"`
	CorrosionAPIToken string   `json:"corrosion_api_token,omitempty"`
	Bootstrap         []string `json:"bootstrap,omitempty"`
	HelperImage       string   `json:"helper_image,omitempty"`
}

type ApplyResult struct {
	Network                 string `json:"network"`
	NetworkCIDR             string `json:"network_cidr"`
	Subnet                  string `json:"subnet"`
	ManagementIP            string `json:"management_ip"`
	WGInterface             string `json:"wg_interface"`
	WGPort                  int    `json:"wg_port"`
	AdvertiseEndpoint       string `json:"advertise_endpoint,omitempty"`
	CorrosionName           string `json:"corrosion_name"`
	CorrosionAPIAddr        string `json:"corrosion_api_addr"`
	CorrosionGossipAddrPort string `json:"corrosion_gossip_addr"`
	DockerNetwork           string `json:"docker_network"`
	SupervisorRunning       bool   `json:"supervisor_running"`
}

type ClockHealth struct {
	NTPOffsetMs float64 `json:"ntp_offset_ms"`
	NTPHealthy  bool    `json:"ntp_healthy"`
	NTPError    string  `json:"ntp_error,omitempty"`
}

// StateNode is a normalized runtime state-machine node.
type StateNode struct {
	Component     string      `json:"component"`
	Phase         string      `json:"phase"`
	Required      bool        `json:"required"`
	Healthy       bool        `json:"healthy"`
	LastErrorCode string      `json:"last_error_code,omitempty"`
	LastError     string      `json:"last_error,omitempty"`
	Hint          string      `json:"hint,omitempty"`
	Children      []StateNode `json:"children,omitempty"`
}

type NetworkStatus struct {
	Configured        bool        `json:"configured"`
	Running           bool        `json:"running"`
	WireGuard         bool        `json:"wireguard"`
	Corrosion         bool        `json:"corrosion"`
	DockerNet         bool        `json:"docker"`
	StatePath         string      `json:"state_path"`
	SupervisorRunning bool        `json:"supervisor_running"`
	ClockHealth       ClockHealth `json:"clock_health"`
	NetworkPhase      string      `json:"network_phase,omitempty"`
	SupervisorPhase   string      `json:"supervisor_phase,omitempty"`
	SupervisorError   string      `json:"supervisor_error,omitempty"`
	ClockPhase        string      `json:"clock_phase,omitempty"`
	DockerRequired    bool        `json:"docker_required"`
	RuntimeTree       StateNode   `json:"runtime_tree,omitempty"`
}

type Identity struct {
	ID                  string `json:"id"`
	PublicKey           string `json:"public_key"`
	Subnet              string `json:"subnet"`
	ManagementIP        string `json:"management_ip"`
	AdvertiseEndpoint   string `json:"advertise_endpoint,omitempty"`
	NetworkCIDR         string `json:"network_cidr,omitempty"`
	WGInterface         string `json:"wg_interface,omitempty"`
	WGPort              int    `json:"wg_port,omitempty"`
	HelperName          string `json:"helper_name,omitempty"`
	CorrosionGossipPort int    `json:"corrosion_gossip_port,omitempty"`
	CorrosionMemberID   uint64 `json:"corrosion_member_id,omitempty"`
	CorrosionAPIToken   string `json:"corrosion_api_token,omitempty"`
	Running             bool   `json:"running"`
}

type MachineEntry struct {
	ID             string        `json:"id"`
	PublicKey      string        `json:"public_key"`
	Subnet         string        `json:"subnet"`
	ManagementIP   string        `json:"management_ip"`
	Endpoint       string        `json:"endpoint"`
	LastUpdated    string        `json:"last_updated,omitempty"`
	Version        int64         `json:"version,omitempty"`
	Freshness      time.Duration `json:"freshness,omitempty"`
	Stale          bool          `json:"stale,omitempty"`
	ReplicationLag time.Duration `json:"replication_lag,omitempty"`
}

type PeerLag struct {
	NodeID         string        `json:"node_id"`
	Freshness      time.Duration `json:"freshness"`
	Stale          bool          `json:"stale"`
	ReplicationLag time.Duration `json:"replication_lag"`
	PingRTT        time.Duration `json:"ping_rtt"` // -1 = unreachable, 0 = no data
}

type PeerHealthResponse struct {
	NodeID      string      `json:"node_id"`
	MachineAddr string      `json:"machine_addr,omitempty"`
	MachineID   string      `json:"machine_id,omitempty"`
	Error       string      `json:"error,omitempty"`
	NTP         ClockHealth `json:"ntp"`
	Peers       []PeerLag   `json:"peers"`
}
