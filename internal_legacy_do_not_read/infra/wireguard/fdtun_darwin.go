//go:build darwin

package wireguard

import (
	"fmt"
	"io"
	"os"
	"strings"
	"sync"

	"golang.org/x/sys/unix"
	"golang.zx2c4.com/wireguard/tun"
)

type fdTUN struct {
	file      *os.File
	ifaceName string
	mtu       int
	events    chan tun.Event
	closeOnce sync.Once
}

func (t *fdTUN) File() *os.File {
	return t.file
}

func (t *fdTUN) Read(bufs [][]byte, sizes []int, offset int) (int, error) {
	if len(bufs) == 0 || len(sizes) == 0 {
		return 0, io.ErrShortBuffer
	}
	if offset < 4 {
		return 0, io.ErrShortBuffer
	}
	if len(bufs[0]) < offset {
		return 0, io.ErrShortBuffer
	}

	n, err := t.file.Read(bufs[0][offset-4:])
	if n < 4 {
		if err != nil {
			return 0, err
		}
		return 0, io.ErrUnexpectedEOF
	}
	sizes[0] = n - 4
	return 1, err
}

func (t *fdTUN) Write(bufs [][]byte, offset int) (int, error) {
	if offset < 4 {
		return 0, io.ErrShortBuffer
	}

	for i, pkt := range bufs {
		if len(pkt) <= offset {
			return i, io.ErrShortBuffer
		}
		frame := pkt[offset-4:]
		frame[0] = 0
		frame[1] = 0
		frame[2] = 0

		switch frame[4] >> 4 {
		case 4:
			frame[3] = unix.AF_INET
		case 6:
			frame[3] = unix.AF_INET6
		default:
			return i, unix.EAFNOSUPPORT
		}

		if _, err := t.file.Write(frame); err != nil {
			return i, err
		}
	}

	return len(bufs), nil
}

func (t *fdTUN) MTU() (int, error) {
	if t.ifaceName != "" {
		fd, err := unix.Socket(unix.AF_INET, unix.SOCK_DGRAM, 0)
		if err == nil {
			defer unix.Close(fd)
			ifr, ioctlErr := unix.IoctlGetIfreqMTU(fd, t.ifaceName)
			if ioctlErr == nil {
				t.mtu = int(ifr.MTU)
				return int(ifr.MTU), nil
			}
		}
	}
	if t.mtu > 0 {
		return t.mtu, nil
	}
	if t.ifaceName == "" {
		return 0, fmt.Errorf("wireguard tun mtu unavailable: missing interface name")
	}
	return 0, fmt.Errorf("wireguard tun mtu unavailable for %s", t.ifaceName)
}

func (t *fdTUN) Name() (string, error) {
	if t.ifaceName == "" {
		return "", fmt.Errorf("wireguard tun interface name unavailable")
	}
	return t.ifaceName, nil
}

func (t *fdTUN) Events() <-chan tun.Event {
	return t.events
}

func (t *fdTUN) Close() error {
	var err error
	t.closeOnce.Do(func() {
		close(t.events)
		err = t.file.Close()
	})
	return err
}

func (t *fdTUN) BatchSize() int {
	return 1
}

func newFDTUN(file *os.File, ifaceName string, mtu int) *fdTUN {
	return &fdTUN{
		file:      file,
		ifaceName: strings.TrimSpace(ifaceName),
		mtu:       mtu,
		events:    make(chan tun.Event),
	}
}
