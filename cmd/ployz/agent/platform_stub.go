//go:build !darwin && !linux

package agent

import (
	"context"
	"fmt"
	"runtime"
)

type stubService struct{}

func NewPlatformService() PlatformService {
	return &stubService{}
}

func (s *stubService) Install(context.Context, InstallConfig) error {
	return fmt.Errorf("agent install is not supported on %s", runtime.GOOS)
}

func (s *stubService) Uninstall(context.Context) error {
	return fmt.Errorf("agent uninstall is not supported on %s", runtime.GOOS)
}

func (s *stubService) Status(context.Context) (ServiceStatus, error) {
	return ServiceStatus{}, fmt.Errorf("agent status is not supported on %s", runtime.GOOS)
}
