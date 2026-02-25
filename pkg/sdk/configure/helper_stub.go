//go:build !darwin && !linux

package configure

import (
	"context"
	"fmt"
	"net"
	"runtime"
)

type stubHelperService struct{}

func newPlatformHelperService() HelperService {
	return &stubHelperService{}
}

func (s *stubHelperService) Configure(context.Context, HelperOptions) error {
	return fmt.Errorf("configure is only supported on Linux/macOS (current: %s)", runtime.GOOS)
}

func (s *stubHelperService) Status(context.Context) (HelperStatus, error) {
	return HelperStatus{}, nil
}

func dialUnixSocket(_ string) (net.Conn, error) {
	return nil, fmt.Errorf("unix sockets unsupported on %s", runtime.GOOS)
}
