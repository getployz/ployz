package ployz

import (
	"net/netip"
	"time"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

// Machine is the runtime status of the local machine.
type Machine struct {
	ID           string
	Name         string
	PublicKey    string
	NetworkPhase string
	Version      string
}

// MachineRecord is a row in the distributed machines table.
// Each machine owns and writes its own record.
type MachineRecord struct {
	ID        string
	Name      string
	PublicKey wgtypes.Key
	Endpoints []netip.AddrPort
	OverlayIP netip.Addr
	Labels    map[string]string
	UpdatedAt time.Time
}

// IsPublic reports whether the machine has at least one public (non-private) endpoint.
func (r MachineRecord) IsPublic() bool {
	for _, ep := range r.Endpoints {
		if !ep.Addr().IsPrivate() && !ep.Addr().IsLoopback() && !ep.Addr().IsLinkLocalUnicast() {
			return true
		}
	}
	return false
}

// MachineEventKind describes what happened to a machine record.
type MachineEventKind uint8

const (
	MachineAdded MachineEventKind = iota + 1
	MachineUpdated
	MachineRemoved
)

// MachineEvent is a single change to a machine record.
type MachineEvent struct {
	Kind   MachineEventKind
	Record MachineRecord
}
