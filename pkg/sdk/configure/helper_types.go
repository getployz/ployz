package configure

import "context"

type HelperOptions struct {
	TUNSocketPath  string
	PrivSocketPath string
	TokenPath      string
	MTU            int
}

type HelperStatus struct {
	Installed bool
	Running   bool
}

type HelperService interface {
	Configure(ctx context.Context, opts HelperOptions) error
	Status(ctx context.Context) (HelperStatus, error)
}
