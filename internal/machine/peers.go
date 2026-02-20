package machine

import (
	"fmt"
	"net"
	"net/netip"
	"strings"
	"time"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

const peerKeepalive = 25 * time.Second

type Peer struct {
	PublicKey    string   `json:"public_key"`
	Subnet       string   `json:"subnet,omitempty"`
	Management   string   `json:"management,omitempty"`
	Endpoint     string   `json:"endpoint,omitempty"`
	AllEndpoints []string `json:"all_endpoints,omitempty"`
}

type peerSpec struct {
	publicKeyString string
	publicKey       wgtypes.Key
	endpoint        *netip.AddrPort
	allowedPrefixes []netip.Prefix
	allowedIPNets   []net.IPNet
	subnet          *netip.Prefix
}

func parsePeerSpec(in Peer) (peerSpec, error) {
	pubKeyStr := strings.TrimSpace(in.PublicKey)
	if pubKeyStr == "" {
		return peerSpec{}, fmt.Errorf("public key is required")
	}
	key, err := wgtypes.ParseKey(pubKeyStr)
	if err != nil {
		return peerSpec{}, fmt.Errorf("parse public key: %w", err)
	}

	subnetStr := strings.TrimSpace(in.Subnet)
	mgmtStr := strings.TrimSpace(in.Management)
	if subnetStr == "" && mgmtStr == "" {
		return peerSpec{}, fmt.Errorf("peer subnet or management ip is required")
	}

	spec := peerSpec{publicKeyString: pubKeyStr, publicKey: key}

	if mgmtStr != "" {
		ip, err := netip.ParseAddr(mgmtStr)
		if err != nil {
			return peerSpec{}, fmt.Errorf("parse management ip: %w", err)
		}
		spec.allowedPrefixes = append(spec.allowedPrefixes, singleIPPrefix(ip))
	}
	if subnetStr != "" {
		subnet, err := netip.ParsePrefix(subnetStr)
		if err != nil {
			return peerSpec{}, fmt.Errorf("parse peer subnet: %w", err)
		}
		spec.subnet = &subnet
		spec.allowedPrefixes = append(spec.allowedPrefixes, subnet)
	}

	epStr := strings.TrimSpace(in.Endpoint)
	if epStr == "" && len(in.AllEndpoints) > 0 {
		epStr = strings.TrimSpace(in.AllEndpoints[0])
	}
	if epStr != "" {
		ep, err := netip.ParseAddrPort(epStr)
		if err != nil {
			return peerSpec{}, fmt.Errorf("parse endpoint: %w", err)
		}
		spec.endpoint = &ep
	}

	spec.allowedIPNets = make([]net.IPNet, len(spec.allowedPrefixes))
	for i, pref := range spec.allowedPrefixes {
		spec.allowedIPNets[i] = prefixToIPNet(pref)
	}
	return spec, nil
}

func buildPeerSpecs(peers []Peer) ([]peerSpec, error) {
	out := make([]peerSpec, 0, len(peers))
	for _, p := range peers {
		spec, err := parsePeerSpec(p)
		if err != nil {
			return nil, err
		}
		out = append(out, spec)
	}
	return out, nil
}

func singleIPPrefix(addr netip.Addr) netip.Prefix {
	if addr.Is6() {
		return netip.PrefixFrom(addr, 128)
	}
	return netip.PrefixFrom(addr, 32)
}

func prefixToIPNet(pref netip.Prefix) net.IPNet {
	bits := 32
	if pref.Addr().Is6() {
		bits = 128
	}
	return net.IPNet{IP: pref.Addr().AsSlice(), Mask: net.CIDRMask(pref.Bits(), bits)}
}

func ipNetToPrefix(n net.IPNet) (netip.Prefix, error) {
	a, ok := netip.AddrFromSlice(n.IP)
	if !ok {
		return netip.Prefix{}, fmt.Errorf("invalid IP %v", n.IP)
	}
	one, _ := n.Mask.Size()
	return netip.PrefixFrom(a.Unmap(), one), nil
}
