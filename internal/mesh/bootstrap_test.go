package mesh

import "testing"

func TestNormalizeBootstrapAddrPortTrimsSpace(t *testing.T) {
	got := NormalizeBootstrapAddrPort("  [fd8c:88ad:7f06:6fa8:a9a6:b2f6:2302:cde1]:53094  ")
	if want := "[fd8c:88ad:7f06:6fa8:a9a6:b2f6:2302:cde1]:53094"; got != want {
		t.Fatalf("NormalizeBootstrapAddrPort() = %q, want %q", got, want)
	}
}

func TestNormalizeBootstrapAddrs(t *testing.T) {
	got := normalizeBootstrapAddrs([]string{
		" [fd8c:88ad:7f06:6fa8:a9a6:b2f6:2302:cde1]:53094 ",
		"[fd8c:88ad:7f06:6fa8:a9a6:b2f6:2302:cde1]:53094",
		"",
		"5.9.85.203:53094",
	})

	if len(got) != 2 {
		t.Fatalf("normalizeBootstrapAddrs() len = %d, want 2 (%v)", len(got), got)
	}
	if got[0] != "[fd8c:88ad:7f06:6fa8:a9a6:b2f6:2302:cde1]:53094" {
		t.Fatalf("normalizeBootstrapAddrs()[0] = %q", got[0])
	}
	if got[1] != "5.9.85.203:53094" {
		t.Fatalf("normalizeBootstrapAddrs()[1] = %q", got[1])
	}
}
