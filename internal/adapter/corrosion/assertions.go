package corrosion

import (
	"ployz/internal/deploy"
	"ployz/internal/network"
	"ployz/internal/supervisor"
)

var (
	_ network.Registry       = Store{}
	_ supervisor.Registry    = Store{}
	_ deploy.ContainerStore  = Store{}
	_ deploy.DeploymentStore = Store{}
)
