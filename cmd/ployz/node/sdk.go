package node

import (
	"context"

	"ployz/cmd/ployz/cmdutil"
	sdkmachine "ployz/pkg/sdk/machine"
)

func service(ctx context.Context, cf *cmdutil.ClusterFlags) (string, *sdkmachine.Service, error) {
	name, api, _, err := cf.DialService(ctx)
	if err != nil {
		return "", nil, err
	}
	return name, sdkmachine.New(api), nil
}
