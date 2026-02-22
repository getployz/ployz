package container

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"net/netip"
	"path/filepath"
	"testing"

	"ployz/internal/adapter/wireguard"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

func TestMigrateLegacyStoreAddresses(t *testing.T) {
	ctx := context.Background()
	dir := t.TempDir()
	dbPath := filepath.Join(dir, "store.db")

	db, err := sql.Open("sqlite", dbPath)
	if err != nil {
		t.Fatalf("open sqlite: %v", err)
	}
	defer db.Close()

	if _, err := db.Exec(`CREATE TABLE machines (id TEXT PRIMARY KEY, public_key TEXT NOT NULL, management_ip TEXT NOT NULL)`); err != nil {
		t.Fatalf("create machines table: %v", err)
	}
	if _, err := db.Exec(`CREATE TABLE __corro_members (actor_id BLOB PRIMARY KEY, address TEXT NOT NULL, foca_state JSON)`); err != nil {
		t.Fatalf("create __corro_members table: %v", err)
	}

	priv, err := wgtypes.GeneratePrivateKey()
	if err != nil {
		t.Fatalf("GeneratePrivateKey(): %v", err)
	}
	publicKey := priv.PublicKey()
	legacy := legacyManagementFromWGKey(publicKey)
	modern := wireguard.ManagementIPFromWGKey(publicKey)

	if _, err := db.Exec(`INSERT INTO machines (id, public_key, management_ip) VALUES (?, ?, ?)`, "node-a", publicKey.String(), legacy.String()); err != nil {
		t.Fatalf("insert machines row: %v", err)
	}
	focaState := fmt.Sprintf(`{"id":{"id":"node-a","addr":"[%s]:53094","ts":1,"cluster_id":0},"incarnation":0,"state":"Down"}`,
		legacy.String(),
	)
	if _, err := db.Exec(`INSERT INTO __corro_members (actor_id, address, foca_state) VALUES (?, ?, ?)`, []byte{0x01}, fmt.Sprintf("[%s]:53094", legacy), focaState); err != nil {
		t.Fatalf("insert __corro_members row: %v", err)
	}

	migrated, err := migrateLegacyStoreAddresses(ctx, dir)
	if err != nil {
		t.Fatalf("migrateLegacyStoreAddresses() error = %v", err)
	}
	if !migrated {
		t.Fatal("migrateLegacyStoreAddresses() migrated = false, want true")
	}

	var mgmtIP string
	if err := db.QueryRow(`SELECT management_ip FROM machines WHERE id = ?`, "node-a").Scan(&mgmtIP); err != nil {
		t.Fatalf("query migrated machine row: %v", err)
	}
	if mgmtIP != modern.String() {
		t.Fatalf("migrated management_ip = %q, want %q", mgmtIP, modern.String())
	}

	var addr string
	var state string
	if err := db.QueryRow(`SELECT address, foca_state FROM __corro_members WHERE actor_id = ?`, []byte{0x01}).Scan(&addr, &state); err != nil {
		t.Fatalf("query migrated member row: %v", err)
	}
	wantAddr := fmt.Sprintf("[%s]:53094", modern.String())
	if addr != wantAddr {
		t.Fatalf("migrated address = %q, want %q", addr, wantAddr)
	}

	var payload struct {
		ID struct {
			Addr string `json:"addr"`
		} `json:"id"`
	}
	if err := json.Unmarshal([]byte(state), &payload); err != nil {
		t.Fatalf("unmarshal migrated foca_state: %v", err)
	}
	if payload.ID.Addr != wantAddr {
		t.Fatalf("migrated foca_state addr = %q, want %q", payload.ID.Addr, wantAddr)
	}
}

func TestMigrateLegacyStoreAddressesNoDB(t *testing.T) {
	migrated, err := migrateLegacyStoreAddresses(context.Background(), t.TempDir())
	if err != nil {
		t.Fatalf("migrateLegacyStoreAddresses() error = %v", err)
	}
	if migrated {
		t.Fatal("migrateLegacyStoreAddresses() migrated = true, want false")
	}
}

func legacyManagementFromWGKey(publicKey wgtypes.Key) netip.Addr {
	var bytes [16]byte
	bytes[0] = 0xfd
	bytes[1] = 0xcc
	copy(bytes[2:], publicKey[:14])
	return netip.AddrFrom16(bytes)
}
