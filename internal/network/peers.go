package network

import (
	"fmt"
	"net/netip"
	"strings"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

type Peer struct {
	PublicKey  string
	Subnet     string
	Management string
	Endpoint   string
}

// PeerSpec holds resolved WireGuard peer parameters.
type PeerSpec struct {
	PublicKey       wgtypes.Key
	Endpoint        *netip.AddrPort
	AllowedPrefixes []netip.Prefix
}

func parsePeerSpec(in Peer) (PeerSpec, error) {
	pubKeyStr := strings.TrimSpace(in.PublicKey)
	if pubKeyStr == "" {
		return PeerSpec{}, fmt.Errorf("public key is required")
	}
	key, err := wgtypes.ParseKey(pubKeyStr)
	if err != nil {
		return PeerSpec{}, fmt.Errorf("parse public key: %w", err)
	}

	subnetStr := strings.TrimSpace(in.Subnet)
	mgmtStr := strings.TrimSpace(in.Management)
	if subnetStr == "" && mgmtStr == "" {
		return PeerSpec{}, fmt.Errorf("peer subnet or management ip is required")
	}

	spec := PeerSpec{PublicKey: key}

	if mgmtStr != "" {
		ip, err := netip.ParseAddr(mgmtStr)
		if err != nil {
			return PeerSpec{}, fmt.Errorf("parse management ip: %w", err)
		}
		spec.AllowedPrefixes = append(spec.AllowedPrefixes, SingleIPPrefix(ip))
	}
	if subnetStr != "" {
		subnet, err := netip.ParsePrefix(subnetStr)
		if err != nil {
			return PeerSpec{}, fmt.Errorf("parse peer subnet: %w", err)
		}
		spec.AllowedPrefixes = append(spec.AllowedPrefixes, subnet)
	}

	epStr := strings.TrimSpace(in.Endpoint)
	if epStr != "" {
		ep, err := netip.ParseAddrPort(epStr)
		if err != nil {
			return PeerSpec{}, fmt.Errorf("parse endpoint: %w", err)
		}
		spec.Endpoint = &ep
	}

	return spec, nil
}

func BuildPeerSpecs(peers []Peer) ([]PeerSpec, error) {
	out := make([]PeerSpec, 0, len(peers))
	for _, p := range peers {
		spec, err := parsePeerSpec(p)
		if err != nil {
			return nil, err
		}
		out = append(out, spec)
	}
	return out, nil
}

// SingleIPPrefix returns a /32 or /128 prefix for a single IP.
func SingleIPPrefix(addr netip.Addr) netip.Prefix {
	if addr.Is6() {
		return netip.PrefixFrom(addr, 128)
	}
	return netip.PrefixFrom(addr, 32)
}

// MachineIP returns the first host IP in a subnet prefix.
func MachineIP(subnet netip.Prefix) netip.Addr {
	return subnet.Masked().Addr().Next()
}
