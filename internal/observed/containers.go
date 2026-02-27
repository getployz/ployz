package observed

import (
	"context"
	"time"
)

// ContainerRecord captures machine-observed runtime container state.
type ContainerRecord struct {
	MachineID     string
	Namespace     string
	DeployID      string
	ContainerName string
	Image         string
	Running       bool
	Healthy       bool
	Ports         []ContainerPort
	ObservedAt    string
}

// ContainerPort captures one observed host-to-container port binding.
type ContainerPort struct {
	HostIP        string
	HostPort      uint16
	ContainerPort uint16
	Protocol      string
}

// ContainerStore persists and reads observed runtime container state.
type ContainerStore interface {
	ReplaceNamespaceSnapshot(ctx context.Context, dataDir, machineID, namespace string, rows []ContainerRecord, observedAt time.Time) error
	ListNamespace(ctx context.Context, dataDir, namespace string) ([]ContainerRecord, error)
}
