package machine

import (
	"fmt"

	"ployz/cmd/ployz/cmdutil"
	"ployz/pkg/sdk/client"
	sdkmachine "ployz/pkg/sdk/machine"
)

func service(socketPath string) (*sdkmachine.Service, error) {
	resolved, err := cmdutil.ResolveSocketPath(socketPath)
	if err != nil {
		return nil, err
	}
	api, err := client.NewUnix(resolved)
	if err != nil {
		return nil, fmt.Errorf("connect to daemon: %w", err)
	}
	return sdkmachine.New(api), nil
}
