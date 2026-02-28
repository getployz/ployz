//go:build darwin

package wireguard

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
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

func ListenForTUN(ctx context.Context, socketPath string) error {
	path := strings.TrimSpace(socketPath)
	if path == "" {
		return fmt.Errorf("tun socket path is required")
	}

	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return fmt.Errorf("create tun socket dir: %w", err)
	}
	if err := os.Remove(path); err != nil && !errors.Is(err, os.ErrNotExist) {
		return fmt.Errorf("remove stale tun socket: %w", err)
	}

	fd, err := unix.Socket(unix.AF_UNIX, unix.SOCK_DGRAM, 0)
	if err != nil {
		return fmt.Errorf("open tun listener socket: %w", err)
	}
	if err := unix.Bind(fd, &unix.SockaddrUnix{Name: path}); err != nil {
		unix.Close(fd)
		return fmt.Errorf("bind tun listener socket: %w", err)
	}
	if err := os.Chmod(path, 0o600); err != nil {
		unix.Close(fd)
		_ = os.Remove(path)
		return fmt.Errorf("set tun listener socket permissions: %w", err)
	}

	log := slog.With("component", "wireguard-darwin", "socket", path)
	log.Debug("tun fd listener started")

	go func() {
		<-ctx.Done()
		_ = unix.Close(fd)
	}()

	go func() {
		defer func() {
			_ = unix.Close(fd)
			_ = os.Remove(path)
			log.Debug("tun fd listener stopped")
		}()

		payload := make([]byte, 1024)
		oob := make([]byte, unix.CmsgSpace(4))

		for {
			n, oobn, flags, _, err := unix.Recvmsg(fd, payload, oob, 0)
			if err != nil {
				if ctx.Err() != nil || errors.Is(err, unix.EBADF) {
					return
				}
				if errors.Is(err, unix.EINTR) {
					continue
				}
				log.Warn("receive tun descriptor failed", "err", err)
				continue
			}
			if flags&(unix.MSG_TRUNC|unix.MSG_CTRUNC) != 0 {
				log.Warn("discarding truncated tun descriptor message")
				continue
			}

			file, name, mtu, privPath, privSecret, recvErr := decodeTUNMessage(payload[:n], oob[:oobn])
			if recvErr != nil {
				log.Warn("discarding invalid tun descriptor message", "err", recvErr)
				continue
			}
			if err := installPrivilegedBroker(privPath, privSecret); err != nil {
				_ = file.Close()
				log.Warn("store privileged helper config failed", "err", err)
				continue
			}
			if err := installProvisionedTUN(file, name, mtu); err != nil {
				_ = file.Close()
				log.Warn("store tun descriptor failed", "err", err)
				continue
			}

			log.Info("received tun descriptor", "iface", name, "mtu", mtu)
		}
	}()

	return nil
}

func decodeTUNMessage(payload []byte, oob []byte) (*os.File, string, int, string, string, error) {
	fds, err := extractRightsFDs(oob)
	if err != nil {
		return nil, "", 0, "", "", err
	}
	if len(fds) == 0 {
		return nil, "", 0, "", "", fmt.Errorf("missing fd rights")
	}

	msg := tunPassMessage{}
	if err := json.Unmarshal(payload, &msg); err != nil {
		closeFDs(fds)
		return nil, "", 0, "", "", fmt.Errorf("decode tun payload: %w", err)
	}
	if msg.Version != tunPassVersion {
		closeFDs(fds)
		return nil, "", 0, "", "", fmt.Errorf("unsupported tun payload version %d", msg.Version)
	}
	msg.Name = strings.TrimSpace(msg.Name)
	if msg.Name == "" {
		closeFDs(fds)
		return nil, "", 0, "", "", fmt.Errorf("tun interface name is required")
	}
	if msg.MTU <= 0 {
		closeFDs(fds)
		return nil, "", 0, "", "", fmt.Errorf("invalid tun mtu %d", msg.MTU)
	}
	msg.PrivSocketPath = strings.TrimSpace(msg.PrivSocketPath)
	if msg.PrivSocketPath == "" {
		closeFDs(fds)
		return nil, "", 0, "", "", fmt.Errorf("privileged helper socket path is required")
	}
	msg.PrivSocketSecret = strings.TrimSpace(msg.PrivSocketSecret)
	if msg.PrivSocketSecret == "" {
		closeFDs(fds)
		return nil, "", 0, "", "", fmt.Errorf("privileged helper socket secret is required")
	}

	for _, extra := range fds[1:] {
		_ = unix.Close(extra)
	}

	file := os.NewFile(uintptr(fds[0]), msg.Name)
	if file == nil {
		_ = unix.Close(fds[0])
		return nil, "", 0, "", "", fmt.Errorf("wrap tun fd")
	}

	return file, msg.Name, msg.MTU, msg.PrivSocketPath, msg.PrivSocketSecret, nil
}

func extractRightsFDs(oob []byte) ([]int, error) {
	cms, err := unix.ParseSocketControlMessage(oob)
	if err != nil {
		return nil, fmt.Errorf("parse socket control messages: %w", err)
	}

	all := make([]int, 0, 1)
	for _, cm := range cms {
		fds, parseErr := unix.ParseUnixRights(&cm)
		if parseErr != nil {
			closeFDs(all)
			return nil, fmt.Errorf("parse unix rights: %w", parseErr)
		}
		all = append(all, fds...)
	}

	return all, nil
}

func closeFDs(fds []int) {
	for _, fd := range fds {
		_ = unix.Close(fd)
	}
}
