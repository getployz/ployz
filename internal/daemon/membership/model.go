package membership

import "ployz/internal/daemon/overlay"

var ErrConflict = overlay.ErrConflict

type MachineRow = overlay.MachineRow
type HeartbeatRow = overlay.HeartbeatRow

type ChangeKind = overlay.ChangeKind

const (
	ChangeAdded   = overlay.ChangeAdded
	ChangeUpdated = overlay.ChangeUpdated
	ChangeDeleted = overlay.ChangeDeleted
	ChangeResync  = overlay.ChangeResync
)

type MachineChange = overlay.MachineChange
type HeartbeatChange = overlay.HeartbeatChange
