package network

import "errors"

// ErrConflict is returned when an optimistic-concurrency version check fails.
var ErrConflict = errors.New("registry version conflict")

// MachineRow represents a machine record in the registry.
type MachineRow struct {
	ID           string
	PublicKey    string
	Subnet       string
	ManagementIP string
	Endpoint     string
	UpdatedAt    string
	Version      int64
}

// HeartbeatRow represents a heartbeat record in the registry.
type HeartbeatRow struct {
	NodeID    string
	Seq       int64
	UpdatedAt string
}

// ChangeKind describes the type of a subscription change event.
type ChangeKind int

const (
	ChangeAdded ChangeKind = iota
	ChangeUpdated
	ChangeDeleted
	ChangeResync
)

// MachineChange is emitted by a machine subscription when a row changes.
type MachineChange struct {
	Kind    ChangeKind
	Machine MachineRow
}

// HeartbeatChange is emitted by a heartbeat subscription when a row changes.
type HeartbeatChange struct {
	Kind      ChangeKind
	Heartbeat HeartbeatRow
}
