package machine

import (
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"ployz/pkg/ipam"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
	_ "modernc.org/sqlite"
)

type State struct {
	Network    string `json:"network"`
	CIDR       string `json:"cidr,omitempty"`
	Subnet     string `json:"subnet"`
	Management string `json:"management"`
	Advertise  string `json:"advertise_endpoint,omitempty"`

	WGInterface string `json:"wg_interface"`
	WGPort      int    `json:"wg_port"`
	WGPrivate   string `json:"wg_private"`
	WGPublic    string `json:"wg_public"`

	DockerNetwork string   `json:"docker_network"`
	CorrosionName string   `json:"corrosion_name"`
	CorrosionImg  string   `json:"corrosion_img"`
	Bootstrap     []string `json:"corrosion_bootstrap,omitempty"`
	Peers         []Peer   `json:"peers,omitempty"`
	Running       bool     `json:"running"`
}

func statePath(dataDir string) string {
	dbPath := machineDBPath(dataDir)
	network := networkFromDataDir(dataDir)
	if network == "" {
		return dbPath
	}
	return dbPath + "#" + network
}

func loadState(dataDir string) (*State, error) {
	network := networkFromDataDir(dataDir)
	if network == "" {
		return nil, fmt.Errorf("resolve network name from data dir %q", dataDir)
	}

	db, err := openMachineDB(machineDBPath(dataDir))
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil, os.ErrNotExist
		}
		return nil, fmt.Errorf("open machine db: %w", err)
	}
	defer db.Close()

	const query = `
SELECT
	network,
	coalesce(cidr, ''),
	subnet,
	management,
	coalesce(advertise, ''),
	wg_interface,
	wg_port,
	wg_private,
	wg_public,
	docker_network,
	corrosion_name,
	corrosion_img,
	coalesce(bootstrap_json, '[]'),
	coalesce(peers_json, '[]'),
	running
FROM network_state
WHERE network = ?`

	row := db.QueryRow(query, network)
	var s State
	var bootstrapJSON string
	var peersJSON string
	var running int
	if err := row.Scan(
		&s.Network,
		&s.CIDR,
		&s.Subnet,
		&s.Management,
		&s.Advertise,
		&s.WGInterface,
		&s.WGPort,
		&s.WGPrivate,
		&s.WGPublic,
		&s.DockerNetwork,
		&s.CorrosionName,
		&s.CorrosionImg,
		&bootstrapJSON,
		&peersJSON,
		&running,
	); err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return nil, os.ErrNotExist
		}
		return nil, fmt.Errorf("read machine state: %w", err)
	}
	s.Running = running != 0

	if err := json.Unmarshal([]byte(bootstrapJSON), &s.Bootstrap); err != nil {
		return nil, fmt.Errorf("parse state bootstrap: %w", err)
	}
	if err := json.Unmarshal([]byte(peersJSON), &s.Peers); err != nil {
		return nil, fmt.Errorf("parse state peers: %w", err)
	}

	managementIP, err := ManagementIPFromPublicKey(s.WGPublic)
	if err != nil {
		return nil, fmt.Errorf("derive management IP from state key: %w", err)
	}
	s.Management = managementIP.String()

	return &s, nil
}

func saveState(dataDir string, s *State) error {
	network := networkFromDataDir(dataDir)
	if network == "" {
		return fmt.Errorf("resolve network name from data dir %q", dataDir)
	}
	if s.Network == "" {
		s.Network = network
	}
	if err := os.MkdirAll(filepath.Dir(machineDBPath(dataDir)), 0o700); err != nil {
		return fmt.Errorf("create machine db dir: %w", err)
	}

	db, err := openMachineDB(machineDBPath(dataDir))
	if err != nil {
		return fmt.Errorf("open machine db: %w", err)
	}
	defer db.Close()

	bootstrapJSON, err := json.Marshal(s.Bootstrap)
	if err != nil {
		return fmt.Errorf("marshal state bootstrap: %w", err)
	}
	peersJSON, err := json.Marshal(s.Peers)
	if err != nil {
		return fmt.Errorf("marshal state peers: %w", err)
	}

	running := 0
	if s.Running {
		running = 1
	}

	const upsert = `
INSERT INTO network_state (
	network,
	cidr,
	subnet,
	management,
	advertise,
	wg_interface,
	wg_port,
	wg_private,
	wg_public,
	docker_network,
	corrosion_name,
	corrosion_img,
	bootstrap_json,
	peers_json,
	running,
	updated_at
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(network) DO UPDATE SET
	cidr = excluded.cidr,
	subnet = excluded.subnet,
	management = excluded.management,
	advertise = excluded.advertise,
	wg_interface = excluded.wg_interface,
	wg_port = excluded.wg_port,
	wg_private = excluded.wg_private,
	wg_public = excluded.wg_public,
	docker_network = excluded.docker_network,
	corrosion_name = excluded.corrosion_name,
	corrosion_img = excluded.corrosion_img,
	bootstrap_json = excluded.bootstrap_json,
	peers_json = excluded.peers_json,
	running = excluded.running,
	updated_at = excluded.updated_at`

	if _, err := db.Exec(
		upsert,
		s.Network,
		s.CIDR,
		s.Subnet,
		s.Management,
		s.Advertise,
		s.WGInterface,
		s.WGPort,
		s.WGPrivate,
		s.WGPublic,
		s.DockerNetwork,
		s.CorrosionName,
		s.CorrosionImg,
		string(bootstrapJSON),
		string(peersJSON),
		running,
		time.Now().UTC().Format(time.RFC3339),
	); err != nil {
		return fmt.Errorf("write machine state: %w", err)
	}
	return nil
}

func deleteState(dataDir string) error {
	network := networkFromDataDir(dataDir)
	if network == "" {
		return fmt.Errorf("resolve network name from data dir %q", dataDir)
	}
	db, err := openMachineDB(machineDBPath(dataDir))
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil
		}
		return fmt.Errorf("open machine db: %w", err)
	}
	defer db.Close()

	if _, err := db.Exec(`DELETE FROM network_state WHERE network = ?`, network); err != nil {
		return fmt.Errorf("delete machine state: %w", err)
	}
	return nil
}

func openMachineDB(path string) (*sql.DB, error) {
	if _, err := os.Stat(path); err != nil {
		if errors.Is(err, os.ErrNotExist) {
			if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
				return nil, fmt.Errorf("create machine db directory: %w", err)
			}
		} else {
			return nil, err
		}
	}

	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, err
	}
	if _, err := db.Exec(`PRAGMA journal_mode = WAL`); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("set sqlite journal mode: %w", err)
	}
	if _, err := db.Exec(`PRAGMA busy_timeout = 5000`); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("set sqlite busy timeout: %w", err)
	}

	const schema = `
CREATE TABLE IF NOT EXISTS network_state (
	network TEXT PRIMARY KEY,
	cidr TEXT,
	subnet TEXT NOT NULL,
	management TEXT NOT NULL,
	advertise TEXT,
	wg_interface TEXT NOT NULL,
	wg_port INTEGER NOT NULL,
	wg_private TEXT NOT NULL,
	wg_public TEXT NOT NULL,
	docker_network TEXT NOT NULL,
	corrosion_name TEXT NOT NULL,
	corrosion_img TEXT NOT NULL,
	bootstrap_json TEXT NOT NULL DEFAULT '[]',
	peers_json TEXT NOT NULL DEFAULT '[]',
	running INTEGER NOT NULL DEFAULT 0,
	updated_at TEXT NOT NULL
)`
	if _, err := db.Exec(schema); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("initialize machine db schema: %w", err)
	}

	return db, nil
}

func machineDBPath(dataDir string) string {
	return filepath.Join(filepath.Dir(filepath.Clean(dataDir)), "machine.db")
}

func networkFromDataDir(dataDir string) string {
	n := filepath.Base(filepath.Clean(dataDir))
	if n == "." || n == "/" {
		return ""
	}
	return n
}

func ensureState(cfg Config) (*State, bool, error) {
	s, err := loadState(cfg.DataDir)
	if err == nil {
		return s, false, nil
	}
	if !errors.Is(err, os.ErrNotExist) {
		return nil, false, err
	}

	priv, err := wgtypes.GeneratePrivateKey()
	if err != nil {
		return nil, false, fmt.Errorf("generate wireguard key: %w", err)
	}

	if !cfg.NetworkCIDR.IsValid() {
		cfg.NetworkCIDR = defaultNetworkPrefix
	}
	if !cfg.Subnet.IsValid() {
		subnet, allocErr := ipam.AllocateSubnet(cfg.NetworkCIDR, nil)
		if allocErr != nil {
			return nil, false, fmt.Errorf("allocate machine subnet: %w", allocErr)
		}
		cfg.Subnet = subnet
	}
	cfg.Management = managementIPFromWGKey(priv.PublicKey())

	s = &State{
		Network:       cfg.Network,
		CIDR:          cfg.NetworkCIDR.String(),
		Subnet:        cfg.Subnet.String(),
		Management:    cfg.Management.String(),
		Advertise:     cfg.AdvertiseEP,
		WGInterface:   cfg.WGInterface,
		WGPort:        cfg.WGPort,
		WGPrivate:     priv.String(),
		WGPublic:      priv.PublicKey().String(),
		DockerNetwork: cfg.DockerNetwork,
		CorrosionName: cfg.CorrosionName,
		CorrosionImg:  cfg.CorrosionImg,
		Bootstrap:     cfg.CorrosionBootstrap,
		Peers:         nil,
		Running:       false,
	}
	if err := saveState(cfg.DataDir, s); err != nil {
		return nil, false, err
	}
	return s, true, nil
}

func LoadState(cfg Config) (*State, error) {
	norm, err := NormalizeConfig(cfg)
	if err != nil {
		return nil, err
	}
	return loadState(norm.DataDir)
}
