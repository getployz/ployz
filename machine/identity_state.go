package machine

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

const identityFileName = "identity.json"

// identityFile is the on-disk format for a machine's identity.
type identityFile struct {
	PrivateKey string `json:"private_key"`
	Name       string `json:"name,omitempty"`
}

// loadOrCreateIdentity reads a machine identity from dataDir, generating a
// new one on first run.
func loadOrCreateIdentity(dataDir string) (Identity, error) {
	path := filepath.Join(dataDir, identityFileName)

	data, err := os.ReadFile(path)
	if err == nil {
		return parseIdentity(data)
	}
	if !errors.Is(err, os.ErrNotExist) {
		return Identity{}, fmt.Errorf("read identity: %w", err)
	}

	id, err := generateIdentity()
	if err != nil {
		return Identity{}, err
	}

	if err := saveIdentity(path, id); err != nil {
		return Identity{}, err
	}

	return id, nil
}

func parseIdentity(data []byte) (Identity, error) {
	var f identityFile
	if err := json.Unmarshal(data, &f); err != nil {
		return Identity{}, fmt.Errorf("parse identity: %w", err)
	}

	key, err := wgtypes.ParseKey(f.PrivateKey)
	if err != nil {
		return Identity{}, fmt.Errorf("parse private key: %w", err)
	}

	return Identity{
		PrivateKey: key,
		Name:       f.Name,
	}, nil
}

func generateIdentity() (Identity, error) {
	key, err := wgtypes.GeneratePrivateKey()
	if err != nil {
		return Identity{}, fmt.Errorf("generate private key: %w", err)
	}

	hostname, _ := os.Hostname() // best-effort; empty name is acceptable

	return Identity{
		PrivateKey: key,
		Name:       hostname,
	}, nil
}

func saveIdentity(path string, id Identity) error {
	f := identityFile{
		PrivateKey: id.PrivateKey.String(),
		Name:       id.Name,
	}

	data, err := json.MarshalIndent(f, "", "  ")
	if err != nil {
		return fmt.Errorf("marshal identity: %w", err)
	}

	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return fmt.Errorf("create identity dir: %w", err)
	}

	if err := os.WriteFile(path, data, 0o600); err != nil {
		return fmt.Errorf("write identity: %w", err)
	}

	return nil
}
