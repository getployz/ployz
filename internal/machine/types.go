package machine

import "net/netip"

type JoinPlan struct {
	NetworkCIDR netip.Prefix
	Subnet      netip.Prefix
	Bootstrap   []string
	LocalSubnet netip.Prefix
	LocalMgmtIP netip.Addr
	LocalWGKey  string
}

type Machine struct {
	ID          string
	PublicKey   string
	Subnet      string
	Management  string
	Endpoint    string
	LastUpdated string
}
