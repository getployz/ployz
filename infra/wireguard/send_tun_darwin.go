//go:build darwin

package wireguard

import (
	"encoding/json"
	"fmt"
	"os"
	"strings"

	"golang.org/x/sys/unix"
)

const tunPassVersion = 1

type tunPassMessage struct {
	Version          int    `json:"version"`
	Name             string `json:"name"`
	MTU              int    `json:"mtu"`
	PrivSocketPath   string `json:"priv_socket_path"`
	PrivSocketSecret string `json:"priv_socket_secret"`
}

func SendTUN(socketPath string, file *os.File, name string, mtu int, privSocketPath, privSocketSecret string) error {
	path := strings.TrimSpace(socketPath)
	if path == "" {
		return fmt.Errorf("tun socket path is required")
	}
	if file == nil {
		return fmt.Errorf("tun file descriptor is required")
	}
	name = strings.TrimSpace(name)
	if name == "" {
		return fmt.Errorf("tun interface name is required")
	}
	if mtu <= 0 {
		return fmt.Errorf("invalid tun mtu %d", mtu)
	}
	privSocketPath = strings.TrimSpace(privSocketPath)
	if privSocketPath == "" {
		return fmt.Errorf("privileged helper socket path is required")
	}
	privSocketSecret = strings.TrimSpace(privSocketSecret)
	if privSocketSecret == "" {
		return fmt.Errorf("privileged helper socket secret is required")
	}

	payload, err := json.Marshal(tunPassMessage{
		Version:          tunPassVersion,
		Name:             name,
		MTU:              mtu,
		PrivSocketPath:   privSocketPath,
		PrivSocketSecret: privSocketSecret,
	})
	if err != nil {
		return fmt.Errorf("marshal tun payload: %w", err)
	}

	fd, err := unix.Socket(unix.AF_UNIX, unix.SOCK_DGRAM, 0)
	if err != nil {
		return fmt.Errorf("open unix datagram socket: %w", err)
	}
	defer unix.Close(fd)

	if err := unix.Connect(fd, &unix.SockaddrUnix{Name: path}); err != nil {
		return fmt.Errorf("connect tun socket %q: %w", path, err)
	}

	rights := unix.UnixRights(int(file.Fd()))
	if err := unix.Sendmsg(fd, payload, rights, nil, 0); err != nil {
		return fmt.Errorf("send tun descriptor: %w", err)
	}

	return nil
}
