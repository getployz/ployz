package types

type NetworkSpec struct {
	Network           string   `json:"network"`
	DataRoot          string   `json:"data_root,omitempty"`
	NetworkCIDR       string   `json:"network_cidr,omitempty"`
	Subnet            string   `json:"subnet,omitempty"`
	ManagementIP      string   `json:"management_ip,omitempty"`
	AdvertiseEndpoint string   `json:"advertise_endpoint,omitempty"`
	WGPort            int      `json:"wg_port,omitempty"`
	Bootstrap         []string `json:"bootstrap,omitempty"`
	HelperImage       string   `json:"helper_image,omitempty"`
}

type ApplyResult struct {
	Network            string `json:"network"`
	NetworkCIDR        string `json:"network_cidr"`
	Subnet             string `json:"subnet"`
	ManagementIP       string `json:"management_ip"`
	WGInterface        string `json:"wg_interface"`
	WGPort             int    `json:"wg_port"`
	AdvertiseEndpoint  string `json:"advertise_endpoint,omitempty"`
	CorrosionName      string `json:"corrosion_name"`
	CorrosionAPIAddr   string `json:"corrosion_api_addr"`
	CorrosionGossipAP  string `json:"corrosion_gossip_addr"`
	DockerNetwork      string `json:"docker_network"`
	ConvergenceRunning bool   `json:"convergence_running"`
}

type NetworkStatus struct {
	Configured    bool   `json:"configured"`
	Running       bool   `json:"running"`
	WireGuard     bool   `json:"wireguard"`
	Corrosion     bool   `json:"corrosion"`
	DockerNet     bool   `json:"docker"`
	StatePath     string `json:"state_path"`
	WorkerRunning bool   `json:"worker_running"`
}

type Identity struct {
	ID                string `json:"id"`
	PublicKey         string `json:"public_key"`
	Subnet            string `json:"subnet"`
	ManagementIP      string `json:"management_ip"`
	AdvertiseEndpoint string `json:"advertise_endpoint,omitempty"`
	NetworkCIDR       string `json:"network_cidr,omitempty"`
	WGInterface       string `json:"wg_interface,omitempty"`
	WGPort            int    `json:"wg_port,omitempty"`
	HelperName        string `json:"helper_name,omitempty"`
	CorrosionGossip   int    `json:"corrosion_gossip_port,omitempty"`
	Running           bool   `json:"running"`
}

type MachineEntry struct {
	ID              string `json:"id"`
	PublicKey       string `json:"public_key"`
	Subnet          string `json:"subnet"`
	ManagementIP    string `json:"management_ip"`
	Endpoint        string `json:"endpoint"`
	LastUpdated     string `json:"last_updated,omitempty"`
	Version         int64  `json:"version,omitempty"`
	ExpectedVersion int64  `json:"expected_version,omitempty"`
}

type Event struct {
	Type    string `json:"type"`
	Network string `json:"network"`
	Message string `json:"message"`
	At      string `json:"at"`
}
