package node

import (
	"context"

	"ployz/cmd/ployz/cmdutil"
	"ployz/pkg/sdk/cluster"
	sdkmachine "ployz/pkg/sdk/machine"
)

func service(ctx context.Context, cf *cmdutil.ClusterFlags) (string, *sdkmachine.Service, cluster.Cluster, error) {
	name, api, cl, err := cf.DialService(ctx)
	if err != nil {
		return "", nil, cluster.Cluster{}, err
	}
	return name, sdkmachine.New(api), cl, nil
}
