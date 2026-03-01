package corrosion

import (
	"ployz/internal/daemon/overlay"
)

var (
	_ overlay.Registry = Store{}
)
