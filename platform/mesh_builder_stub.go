//go:build !linux && !darwin

package platform

import "ployz/machine"

// NewMeshBuilder returns nil on unsupported platforms — network commands
// will fail with a clear error at the daemon layer.
func NewMeshBuilder(_ machine.Identity, _ string) (machine.MeshBuilder, error) {
	return nil, nil
}
