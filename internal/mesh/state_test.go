package mesh

import (
	"encoding/hex"
	"testing"
)

func TestEnsureCorrosionSecurity(t *testing.T) {
	t.Run("both provided returns same values", func(t *testing.T) {
		id, token, err := ensureCorrosionSecurity(42, "existing-token")
		if err != nil {
			t.Fatalf("ensureCorrosionSecurity() error = %v", err)
		}
		if id != 42 {
			t.Errorf("memberID = %d, want 42", id)
		}
		if token != "existing-token" {
			t.Errorf("apiToken = %q, want %q", token, "existing-token")
		}
	})

	t.Run("neither provided generates new", func(t *testing.T) {
		id, token, err := ensureCorrosionSecurity(0, "")
		if err != nil {
			t.Fatalf("ensureCorrosionSecurity() error = %v", err)
		}
		if id == 0 {
			t.Error("memberID should be non-zero")
		}
		if token == "" {
			t.Error("apiToken should be non-empty")
		}
	})

	t.Run("partial: only memberID generates token", func(t *testing.T) {
		id, token, err := ensureCorrosionSecurity(99, "")
		if err != nil {
			t.Fatalf("ensureCorrosionSecurity() error = %v", err)
		}
		if id != 99 {
			t.Errorf("memberID = %d, want 99", id)
		}
		if token == "" {
			t.Error("apiToken should be generated when empty")
		}
		if len(token) != 64 {
			t.Errorf("apiToken len = %d, want 64 hex chars", len(token))
		}
	})
}

func TestGenerateCorrosionMemberID(t *testing.T) {
	t.Run("result non-zero", func(t *testing.T) {
		id, err := generateCorrosionMemberID()
		if err != nil {
			t.Fatalf("generateCorrosionMemberID() error = %v", err)
		}
		if id == 0 {
			t.Error("generateCorrosionMemberID() returned 0")
		}
	})

	t.Run("high bit clear", func(t *testing.T) {
		for i := range 20 {
			id, err := generateCorrosionMemberID()
			if err != nil {
				t.Fatalf("generateCorrosionMemberID() error = %v", err)
			}
			if id>>63 != 0 {
				t.Errorf("iteration %d: high bit set on %d", i, id)
			}
		}
	})
}

func TestGenerateCorrosionAPIToken(t *testing.T) {
	t.Run("result is 64 hex chars", func(t *testing.T) {
		token, err := generateCorrosionAPIToken()
		if err != nil {
			t.Fatalf("generateCorrosionAPIToken() error = %v", err)
		}
		if len(token) != 64 {
			t.Errorf("token len = %d, want 64", len(token))
		}
		// Verify it's valid hex.
		_, err = hex.DecodeString(token)
		if err != nil {
			t.Errorf("token %q is not valid hex: %v", token, err)
		}
	})
}
