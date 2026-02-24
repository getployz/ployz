package manager

import (
	"context"

	"ployz/internal/engine"
	"ployz/internal/network"
	"ployz/pkg/sdk/client"
)

// Compile-time check: Manager implements client.API.
var _ client.API = (*Manager)(nil)

type Manager struct {
	ctx        context.Context
	dataRoot   string
	store      SpecStore
	stateStore network.StateStore
	ctrl       *network.Controller
	engine     *engine.Engine
}

type managerCfg struct {
	specStore  SpecStore
	stateStore network.StateStore
	ctrl       *network.Controller
	eng        *engine.Engine
}

// ManagerOption configures a Manager.
type ManagerOption func(*managerCfg)

// WithSpecStore injects a SpecStore for New.
// NewProduction wires a sqlite-backed store automatically.
func WithSpecStore(s SpecStore) ManagerOption {
	return func(c *managerCfg) { c.specStore = s }
}

// WithManagerStateStore injects a network.StateStore for New.
// NewProduction wires sqlite.NetworkStateStore automatically.
func WithManagerStateStore(s network.StateStore) ManagerOption {
	return func(c *managerCfg) { c.stateStore = s }
}

// WithManagerController injects a pre-built Controller.
func WithManagerController(ctrl *network.Controller) ManagerOption {
	return func(c *managerCfg) { c.ctrl = ctrl }
}

// WithManagerEngine injects a pre-built Engine.
func WithManagerEngine(e *engine.Engine) ManagerOption {
	return func(c *managerCfg) { c.eng = e }
}
