//go:build !linux && !darwin

package platform

import "ployz/machine"

// NewMachine creates a standalone machine on unsupported platforms.
// No mesh builder is configured — network commands will fail.
func NewMachine(dataDir string) (*machine.Machine, error) {
	return machine.New(dataDir)
}
