//go:build darwin || linux

package agent

import (
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
)

const (
	dataRootModeDarwin = 0o775
	dataRootModeUnix   = 0o755
	privateRootMode    = 0o700
)

func reconcileDaemonDataPaths(dataRoot string, uid, gid int, goos string) error {
	root := filepath.Clean(strings.TrimSpace(dataRoot))
	if root == "." {
		return fmt.Errorf("invalid data root path")
	}

	rootDir := filepath.Dir(root)
	privateDir := filepath.Join(rootDir, "private")

	if err := ensureOwnedDir(rootDir, uid, gid, dataRootModeUnix); err != nil {
		return fmt.Errorf("prepare data root parent %q: %w", rootDir, err)
	}

	dataMode := dataRootModeForOS(goos)
	if err := ensureOwnedDir(root, uid, gid, dataMode); err != nil {
		return fmt.Errorf("prepare data root %q: %w", root, err)
	}
	if err := walkDataTree(root, uid, gid, dataMode); err != nil {
		return fmt.Errorf("reconcile data root tree %q: %w", root, err)
	}

	if err := ensureOwnedDir(privateDir, uid, gid, privateRootMode); err != nil {
		return fmt.Errorf("prepare private root %q: %w", privateDir, err)
	}

	return nil
}

func ensureOwnedDir(path string, uid, gid int, mode os.FileMode) error {
	if err := os.MkdirAll(path, mode); err != nil {
		return err
	}
	if err := os.Chown(path, uid, gid); err != nil {
		return err
	}
	if err := os.Chmod(path, mode); err != nil {
		return err
	}
	return nil
}

func walkDataTree(root string, uid, gid int, dirMode os.FileMode) error {
	return filepath.WalkDir(root, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if d.Type()&os.ModeSymlink != 0 {
			return nil
		}
		if err := os.Chown(path, uid, gid); err != nil {
			return err
		}
		if d.IsDir() {
			if err := os.Chmod(path, dirMode); err != nil {
				return err
			}
		}
		return nil
	})
}

func dataRootModeForOS(goos string) os.FileMode {
	if goos == "darwin" {
		return dataRootModeDarwin
	}
	return dataRootModeUnix
}
