package mesh

import "testing"

func FuzzParsePeerSpec(f *testing.F) {
	f.Add("", "10.0.0.0/24", "fd8c:88ad:7f06::1", "5.9.85.203:51820")
	f.Add("dGVzdA==", "", "", "")
	f.Add("invalid", "invalid", "invalid", "invalid")

	f.Fuzz(func(t *testing.T, pubkey, subnet, mgmt, endpoint string) {
		spec, err := parsePeerSpec(Peer{
			PublicKey:  pubkey,
			Subnet:     subnet,
			ManagementIP: mgmt,
			Endpoint:   endpoint,
		})
		if err != nil {
			// Invalid input should not panic.
			return
		}
		// Valid output always has a non-zero public key.
		if spec.PublicKey == ([32]byte{}) {
			t.Error("valid output has zero public key")
		}
		// Valid output always has at least one allowed prefix.
		if len(spec.AllowedPrefixes) == 0 {
			t.Error("valid output has no allowed prefixes")
		}
		// If management was provided, it should produce a valid prefix.
		for _, p := range spec.AllowedPrefixes {
			if !p.IsValid() {
				t.Errorf("invalid allowed prefix: %v", p)
			}
		}
	})
}
