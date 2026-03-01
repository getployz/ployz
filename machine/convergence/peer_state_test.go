package convergence

import (
	"testing"
	"time"

	"ployz"
)

func TestClassifyPeer(t *testing.T) {
	now := time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC)

	tests := []struct {
		name               string
		state              peerState
		want               ployz.PeerHealth
		wantAttempted      int
		wantEndpointIndex  int
	}{
		{
			name: "fresh handshake → Alive",
			state: peerState{
				endpointCount:      3,
				endpointsAttempted: 2,
				endpointIndex:      1,
				lastHandshake:      now.Add(-100 * time.Second),
			},
			want:              ployz.PeerAlive,
			wantAttempted:     0, // reset on alive
			wantEndpointIndex: 1, // sticky
		},
		{
			name: "handshake exactly at freshness boundary → Alive",
			state: peerState{
				endpointCount:      2,
				endpointsAttempted: 1,
				lastHandshake:      now.Add(-aliveFreshness),
			},
			want:          ployz.PeerAlive,
			wantAttempted: 0,
		},
		{
			name: "no handshake, not all endpoints tried → New",
			state: peerState{
				endpointCount:      3,
				endpointsAttempted: 1,
			},
			want:          ployz.PeerNew,
			wantAttempted: 1,
		},
		{
			name: "no handshake, zero attempted of many → New",
			state: peerState{
				endpointCount:      3,
				endpointsAttempted: 0,
			},
			want:          ployz.PeerNew,
			wantAttempted: 0,
		},
		{
			name: "no handshake, all endpoints tried → Suspect",
			state: peerState{
				endpointCount:      3,
				endpointsAttempted: 3,
			},
			want:          ployz.PeerSuspect,
			wantAttempted: 3,
		},
		{
			name: "stale handshake, all endpoints tried → Suspect",
			state: peerState{
				endpointCount:      2,
				endpointsAttempted: 2,
				lastHandshake:      now.Add(-aliveFreshness - time.Second),
			},
			want:          ployz.PeerSuspect,
			wantAttempted: 2,
		},
		{
			name: "stale handshake, not all tried → New",
			state: peerState{
				endpointCount:      3,
				endpointsAttempted: 1,
				lastHandshake:      now.Add(-aliveFreshness - time.Second),
			},
			want:          ployz.PeerNew,
			wantAttempted: 1,
		},
		{
			name: "single endpoint, no handshake, 0 attempted → New",
			state: peerState{
				endpointCount:      1,
				endpointsAttempted: 0,
			},
			want:          ployz.PeerNew,
			wantAttempted: 0,
		},
		{
			name: "single endpoint, no handshake, 1 attempted → Suspect",
			state: peerState{
				endpointCount:      1,
				endpointsAttempted: 1,
			},
			want:          ployz.PeerSuspect,
			wantAttempted: 1,
		},
		{
			name: "fresh handshake resets endpointsAttempted but keeps endpointIndex",
			state: peerState{
				endpointCount:      3,
				endpointIndex:      2,
				endpointsAttempted: 3,
				lastHandshake:      now.Add(-10 * time.Second),
			},
			want:              ployz.PeerAlive,
			wantAttempted:     0,
			wantEndpointIndex: 2,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			s := tt.state
			classifyPeer(&s, now)
			if s.health != tt.want {
				t.Errorf("classifyPeer() health = %v, want %v", s.health, tt.want)
			}
			if s.endpointsAttempted != tt.wantAttempted {
				t.Errorf("endpointsAttempted = %d, want %d", s.endpointsAttempted, tt.wantAttempted)
			}
			if tt.wantEndpointIndex != 0 && s.endpointIndex != tt.wantEndpointIndex {
				t.Errorf("endpointIndex = %d, want %d", s.endpointIndex, tt.wantEndpointIndex)
			}
		})
	}
}

func TestShouldRotate(t *testing.T) {
	now := time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC)

	tests := []struct {
		name  string
		state peerState
		want  bool
	}{
		{
			name: "single endpoint → false",
			state: peerState{
				endpointCount: 1,
				endpointSetAt: now.Add(-30 * time.Second),
			},
			want: false,
		},
		{
			name: "fresh handshake → false",
			state: peerState{
				endpointCount: 3,
				endpointSetAt: now.Add(-30 * time.Second),
				lastHandshake: now.Add(-100 * time.Second),
			},
			want: false,
		},
		{
			name: "stale handshake, endpoint set long ago → true",
			state: peerState{
				endpointCount: 3,
				endpointSetAt: now.Add(-20 * time.Second),
				lastHandshake: now.Add(-300 * time.Second),
			},
			want: true,
		},
		{
			name: "no handshake, endpoint set 16s ago → true",
			state: peerState{
				endpointCount: 2,
				endpointSetAt: now.Add(-16 * time.Second),
			},
			want: true,
		},
		{
			name: "no handshake, endpoint set 14s ago → false",
			state: peerState{
				endpointCount: 2,
				endpointSetAt: now.Add(-14 * time.Second),
			},
			want: false,
		},
		{
			name: "exactly at timeout → true",
			state: peerState{
				endpointCount: 2,
				endpointSetAt: now.Add(-endpointTimeout),
			},
			want: true,
		},
		{
			name: "endpointSetAt zero → false",
			state: peerState{
				endpointCount: 2,
			},
			want: false,
		},
		{
			name: "had handshake within 275s, endpoint old → false",
			state: peerState{
				endpointCount: 3,
				endpointSetAt: now.Add(-60 * time.Second),
				lastHandshake: now.Add(-200 * time.Second),
			},
			want: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			s := tt.state
			got := shouldRotate(&s, now)
			if got != tt.want {
				t.Errorf("shouldRotate() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestNextEndpoint(t *testing.T) {
	tests := []struct {
		name              string
		index             int
		count             int
		attempted         int
		wantIndex         int
		wantAttempted     int
	}{
		{
			name:          "wraps around",
			index:         2,
			count:         3,
			attempted:     2,
			wantIndex:     0,
			wantAttempted: 3,
		},
		{
			name:          "advances normally",
			index:         0,
			count:         3,
			attempted:     0,
			wantIndex:     1,
			wantAttempted: 1,
		},
		{
			name:          "attempted capped at count",
			index:         1,
			count:         2,
			attempted:     2,
			wantIndex:     0,
			wantAttempted: 2,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			s := &peerState{
				endpointIndex:      tt.index,
				endpointCount:      tt.count,
				endpointsAttempted: tt.attempted,
			}
			got := nextEndpoint(s)
			if got != tt.wantIndex {
				t.Errorf("nextEndpoint() = %d, want %d", got, tt.wantIndex)
			}
			if s.endpointsAttempted != tt.wantAttempted {
				t.Errorf("endpointsAttempted = %d, want %d", s.endpointsAttempted, tt.wantAttempted)
			}
		})
	}
}
