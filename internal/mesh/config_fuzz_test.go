package mesh

import "testing"

func FuzzNormalizeConfig(f *testing.F) {
	f.Add("default", "", "5.9.85.203:51820", 51820)
	f.Add("", "", "", 0)
	f.Add("test-network", "/tmp/data", "invalid-endpoint", 9999)

	f.Fuzz(func(t *testing.T, network, dataRoot, advertise string, wgPort int) {
		cfg := Config{
			Network:     network,
			DataRoot:    dataRoot,
			AdvertiseEndpoint: advertise,
			WGPort:      wgPort,
		}

		out, err := NormalizeConfig(cfg)
		if err != nil {
			return
		}

		// Idempotent: Normalize(Normalize(x)) == Normalize(x).
		out2, err2 := NormalizeConfig(out)
		if err2 != nil {
			t.Fatalf("second normalize failed: %v", err2)
		}
		if out.DataDir != out2.DataDir {
			t.Errorf("not idempotent: DataDir %q != %q", out.DataDir, out2.DataDir)
		}
		if out.Network != out2.Network {
			t.Errorf("not idempotent: Network %q != %q", out.Network, out2.Network)
		}

		// DataDir always non-empty on success.
		if out.DataDir == "" {
			t.Error("DataDir should not be empty after normalize")
		}
	})
}
