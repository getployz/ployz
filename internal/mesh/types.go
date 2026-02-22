package mesh

type Machine struct {
	ID          string
	PublicKey   string
	Subnet      string
	Management  string
	Endpoint    string
	LastUpdated string
	Version     int64

	ExpectedVersion int64
}
