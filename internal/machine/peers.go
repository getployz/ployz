package machine

import (
	"fmt"
	"net"
	"net/netip"
	"slices"
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

func normalizePeer(in Peer) (Peer, error) {
	p := Peer{
		PublicKey:  strings.TrimSpace(in.PublicKey),
		Subnet:     strings.TrimSpace(in.Subnet),
		Management: strings.TrimSpace(in.Management),
		Endpoint:   strings.TrimSpace(in.Endpoint),
	}

	for _, ep := range in.AllEndpoints {
		ep = strings.TrimSpace(ep)
		if ep == "" {
			continue
		}
		p.AllEndpoints = append(p.AllEndpoints, ep)
	}

	if p.PublicKey == "" {
		return Peer{}, fmt.Errorf("public key is required")
	}
	if _, err := wgtypes.ParseKey(p.PublicKey); err != nil {
		return Peer{}, fmt.Errorf("parse public key: %w", err)
	}

	if p.Subnet == "" && p.Management == "" {
		return Peer{}, fmt.Errorf("peer subnet or management ip is required")
	}
	if p.Subnet != "" {
		if _, err := netip.ParsePrefix(p.Subnet); err != nil {
			return Peer{}, fmt.Errorf("parse peer subnet: %w", err)
		}
	}
	if p.Management != "" {
		if _, err := netip.ParseAddr(p.Management); err != nil {
			return Peer{}, fmt.Errorf("parse management ip: %w", err)
		}
	}

	if p.Endpoint == "" && len(p.AllEndpoints) > 0 {
		p.Endpoint = p.AllEndpoints[0]
	}
	if p.Endpoint != "" {
		if _, err := netip.ParseAddrPort(p.Endpoint); err != nil {
			return Peer{}, fmt.Errorf("parse endpoint: %w", err)
		}
		if len(p.AllEndpoints) == 0 {
			p.AllEndpoints = []string{p.Endpoint}
		}
		if !slices.Contains(p.AllEndpoints, p.Endpoint) {
			p.AllEndpoints = append([]string{p.Endpoint}, p.AllEndpoints...)
		}
	}

	for _, ep := range p.AllEndpoints {
		if _, err := netip.ParseAddrPort(ep); err != nil {
			return Peer{}, fmt.Errorf("parse endpoint %q: %w", ep, err)
		}
	}

	return p, nil
}

func buildPeerSpecs(peers []Peer) ([]peerSpec, error) {
	out := make([]peerSpec, 0, len(peers))
	for _, raw := range peers {
		p, err := normalizePeer(raw)
		if err != nil {
			return nil, err
		}

		key, err := wgtypes.ParseKey(p.PublicKey)
		if err != nil {
			return nil, fmt.Errorf("parse public key %q: %w", p.PublicKey, err)
		}

		spec := peerSpec{publicKeyString: p.PublicKey, publicKey: key}

		if p.Endpoint != "" {
			ep, err := netip.ParseAddrPort(p.Endpoint)
			if err != nil {
				return nil, fmt.Errorf("parse endpoint %q: %w", p.Endpoint, err)
			}
			spec.endpoint = &ep
		}

		if p.Management != "" {
			ip, err := netip.ParseAddr(p.Management)
			if err != nil {
				return nil, fmt.Errorf("parse management ip %q: %w", p.Management, err)
			}
			spec.allowedPrefixes = append(spec.allowedPrefixes, singleIPPrefix(ip))
		}

		if p.Subnet != "" {
			subnet, err := netip.ParsePrefix(p.Subnet)
			if err != nil {
				return nil, fmt.Errorf("parse peer subnet %q: %w", p.Subnet, err)
			}
			spec.subnet = &subnet
			spec.allowedPrefixes = append(spec.allowedPrefixes, subnet)
		}

		if len(spec.allowedPrefixes) == 0 {
			return nil, fmt.Errorf("peer %q has no allowed IPs", p.PublicKey)
		}

		spec.allowedIPNets = make([]net.IPNet, len(spec.allowedPrefixes))
		for i, pref := range spec.allowedPrefixes {
			spec.allowedIPNets[i] = prefixToIPNet(pref)
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
