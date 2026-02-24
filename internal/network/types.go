package network

type Machine struct {
	ID           string
	PublicKey    string
	Subnet       string
	ManagementIP string
	Endpoint     string
	LastUpdated  string
	Version      int64

	ExpectedVersion int64
}
