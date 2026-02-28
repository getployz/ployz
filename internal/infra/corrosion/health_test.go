package corrosion

import (
	"context"
	"net"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"net/url"
	"strconv"
	"strings"
	"testing"
	"time"
)

func TestProbeHealthPhases(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name            string
		status          int
		body            string
		token           string
		expectedMembers int
		want            HealthPhase
	}{
		{
			name:            "ready on 200 with full membership",
			status:          http.StatusOK,
			body:            `{"response":{"gaps":0,"members":3,"p99_lag":0.12,"queue_size":0}}`,
			token:           "secret-token",
			expectedMembers: 3,
			want:            HealthReady,
		},
		{
			name:            "forming on 200 with low membership",
			status:          http.StatusOK,
			body:            `{"response":{"gaps":0,"members":1,"p99_lag":0.12,"queue_size":0}}`,
			token:           "",
			expectedMembers: 3,
			want:            HealthForming,
		},
		{
			name:            "ready on 200 for single-node remote peer target",
			status:          http.StatusOK,
			body:            `{"response":{"gaps":0,"members":0,"p99_lag":0.12,"queue_size":0}}`,
			token:           "",
			expectedMembers: 0,
			want:            HealthReady,
		},
		{
			name:            "syncing on 503",
			status:          http.StatusServiceUnavailable,
			body:            `{"response":{"gaps":0,"members":3,"p99_lag":7.01,"queue_size":0}}`,
			token:           "",
			expectedMembers: 3,
			want:            HealthSyncing,
		},
		{
			name:            "unreachable on malformed payload",
			status:          http.StatusOK,
			body:            `{"response":{"gaps":"oops","members":3,"queue_size":0}}`,
			token:           "",
			expectedMembers: 3,
			want:            HealthUnreachable,
		},
	}

	for _, tc := range tests {
		tc := tc
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				if got := r.URL.RequestURI(); got != corrosionHealthPath {
					t.Errorf("request uri = %q, want %q", got, corrosionHealthPath)
				}
				if tc.token == "" {
					if got := r.Header.Get("Authorization"); got != "" {
						t.Errorf("unexpected authorization header %q", got)
					}
				} else {
					wantAuth := "Bearer " + tc.token
					if got := r.Header.Get("Authorization"); got != wantAuth {
						t.Errorf("authorization header = %q, want %q", got, wantAuth)
					}
				}
				w.WriteHeader(tc.status)
				_, _ = w.Write([]byte(tc.body))
			}))
			defer srv.Close()

			apiAddr := testServerAddr(t, srv.URL)
			got := ProbeHealth(context.Background(), apiAddr, tc.token, tc.expectedMembers)
			if got != tc.want {
				t.Fatalf("ProbeHealth() = %s, want %s", got, tc.want)
			}
		})
	}
}

func TestProbeHealthUnreachableOnTransportError(t *testing.T) {
	t.Parallel()

	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	addr := ln.Addr().String()
	if err := ln.Close(); err != nil {
		t.Fatalf("close listener: %v", err)
	}

	apiAddr, err := netip.ParseAddrPort(addr)
	if err != nil {
		t.Fatalf("parse addr %q: %v", addr, err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 250*time.Millisecond)
	defer cancel()

	got := ProbeHealth(ctx, apiAddr, "", 1)
	if got != HealthUnreachable {
		t.Fatalf("ProbeHealth() = %s, want %s", got, HealthUnreachable)
	}
}

func testServerAddr(t *testing.T, rawURL string) netip.AddrPort {
	t.Helper()

	parsed, err := url.Parse(rawURL)
	if err != nil {
		t.Fatalf("parse test server URL %q: %v", rawURL, err)
	}
	host, portRaw, err := net.SplitHostPort(parsed.Host)
	if err != nil {
		t.Fatalf("split host/port from %q: %v", parsed.Host, err)
	}
	ip, err := netip.ParseAddr(strings.TrimSpace(host))
	if err != nil {
		t.Fatalf("parse host addr %q: %v", host, err)
	}
	port, err := strconv.Atoi(portRaw)
	if err != nil {
		t.Fatalf("parse host port %q: %v", portRaw, err)
	}
	if port <= 0 || port > 65535 {
		t.Fatalf("invalid test server port %d", port)
	}

	return netip.AddrPortFrom(ip, uint16(port))
}
