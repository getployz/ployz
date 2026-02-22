package mesh

import (
	"net/netip"
	"testing"
)

func FuzzManagementIPFromPublicKey(f *testing.F) {
	f.Add("dGVzdGluZzEyMzQ1Njc4OTAxMjM0NTY3ODkwMTI=")
	f.Add("")
	f.Add("not-base64")

	mgmtPrefix := netip.MustParsePrefix(ManagementCIDR)

	f.Fuzz(func(t *testing.T, input string) {
		addr, err := ManagementIPFromPublicKey(input)
		if err != nil {
			return
		}
		// Same key always produces same IP.
		addr2, err2 := ManagementIPFromPublicKey(input)
		if err2 != nil {
			t.Fatal("second call failed but first succeeded")
		}
		if addr != addr2 {
			t.Errorf("not deterministic: %v != %v", addr, addr2)
		}
		// Result always in fd8c::/48.
		if !mgmtPrefix.Contains(addr) {
			t.Errorf("result %v not in %s", addr, ManagementCIDR)
		}
	})
}
