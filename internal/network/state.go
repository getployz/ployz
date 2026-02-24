package network

import (
	"crypto/rand"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"strings"

	"ployz/pkg/ipam"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
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

	HostWGPrivate string `json:"host_wg_private,omitempty"`
	HostWGPublic  string `json:"host_wg_public,omitempty"`

	DockerNetwork     string              `json:"docker_network"`
	CorrosionName     string              `json:"corrosion_name"`
	CorrosionImage    string              `json:"corrosion_img"`
	CorrosionMemberID uint64              `json:"corrosion_member_id"`
	CorrosionAPIToken string              `json:"corrosion_api_token,omitempty"`
	Bootstrap         []string            `json:"corrosion_bootstrap,omitempty"`
	Phase             NetworkRuntimePhase `json:"phase"`
}

func (s *State) UnmarshalJSON(data []byte) error {
	type stateAlias State
	var aux struct {
		stateAlias
		Running *bool `json:"running"`
	}
	if err := json.Unmarshal(data, &aux); err != nil {
		return err
	}
	*s = State(aux.stateAlias)
	if s.Phase != 0 {
		return nil
	}
	if aux.Running != nil && *aux.Running {
		s.Phase = NetworkRunning
		return nil
	}
	s.Phase = NetworkStopped
	return nil
}

func ensureState(store StateStore, cfg Config) (*State, bool, error) {
	s, err := store.Load(cfg.DataDir)
	if err == nil {
		if s.Phase == 0 {
			s.Phase = NetworkStopped
		}
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
	cfg.Management = ManagementIPFromWGKey(priv.PublicKey())

	memberID, apiToken, err := ensureCorrosionSecurity(cfg.CorrosionMemberID, cfg.CorrosionAPIToken)
	if err != nil {
		return nil, false, err
	}

	s = &State{
		Network:           cfg.Network,
		CIDR:              cfg.NetworkCIDR.String(),
		Subnet:            cfg.Subnet.String(),
		Management:        cfg.Management.String(),
		Advertise:         cfg.AdvertiseEndpoint,
		WGInterface:       cfg.WGInterface,
		WGPort:            cfg.WGPort,
		WGPrivate:         priv.String(),
		WGPublic:          priv.PublicKey().String(),
		DockerNetwork:     cfg.DockerNetwork,
		CorrosionName:     cfg.CorrosionName,
		CorrosionImage:    cfg.CorrosionImage,
		CorrosionMemberID: memberID,
		CorrosionAPIToken: apiToken,
		Bootstrap:         cfg.CorrosionBootstrap,
		Phase:             NetworkStopped,
	}
	if err := store.Save(cfg.DataDir, s); err != nil {
		return nil, false, err
	}
	return s, true, nil
}

// LoadState loads persisted state for a network using the given store.
func LoadState(store StateStore, cfg Config) (*State, error) {
	norm, err := NormalizeConfig(cfg)
	if err != nil {
		return nil, err
	}
	return store.Load(norm.DataDir)
}

func ensureCorrosionSecurity(memberID uint64, apiToken string) (uint64, string, error) {
	if memberID == 0 {
		id, err := generateCorrosionMemberID()
		if err != nil {
			return 0, "", fmt.Errorf("generate corrosion member id: %w", err)
		}
		memberID = id
	}

	apiToken = strings.TrimSpace(apiToken)
	if apiToken == "" {
		token, err := generateCorrosionAPIToken()
		if err != nil {
			return 0, "", fmt.Errorf("generate corrosion api token: %w", err)
		}
		apiToken = token
	}

	return memberID, apiToken, nil
}

func generateCorrosionMemberID() (uint64, error) {
	var raw [8]byte
	if _, err := rand.Read(raw[:]); err != nil {
		return 0, err
	}
	v := binary.LittleEndian.Uint64(raw[:])
	v &^= 1 << 63 // clear high bit — database/sql rejects uint64 > max int64
	if v == 0 {
		v = 1
	}
	return v, nil
}

const corrosionAPITokenLengthBytes = 32 // 32 bytes → 64 hex chars, sufficient entropy for cluster auth

func generateCorrosionAPIToken() (string, error) {
	raw := make([]byte, corrosionAPITokenLengthBytes)
	if _, err := rand.Read(raw); err != nil {
		return "", err
	}
	return hex.EncodeToString(raw), nil
}
