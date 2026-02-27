package corrosion

import "testing"

func TestClassifyHealth(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name            string
		sample          HealthSample
		expectedMembers int
		want            HealthPhase
	}{
		{
			name: "unreachable when probe fails",
			sample: HealthSample{
				Reachable: false,
			},
			expectedMembers: 3,
			want:            HealthUnreachable,
		},
		{
			name: "forming when members below expected",
			sample: HealthSample{
				Reachable:     true,
				ThresholdsMet: true,
				Members:       1,
				Gaps:          0,
				QueueSize:     0,
			},
			expectedMembers: 2,
			want:            HealthForming,
		},
		{
			name: "syncing when threshold check fails",
			sample: HealthSample{
				Reachable:     true,
				ThresholdsMet: false,
				Members:       2,
				Gaps:          0,
				QueueSize:     0,
			},
			expectedMembers: 2,
			want:            HealthSyncing,
		},
		{
			name: "syncing when gaps remain",
			sample: HealthSample{
				Reachable:     true,
				ThresholdsMet: true,
				Members:       2,
				Gaps:          1,
				QueueSize:     0,
			},
			expectedMembers: 2,
			want:            HealthSyncing,
		},
		{
			name: "ready when checks hold",
			sample: HealthSample{
				Reachable:     true,
				ThresholdsMet: true,
				Members:       2,
				Gaps:          0,
				QueueSize:     0,
			},
			expectedMembers: 2,
			want:            HealthReady,
		},
		{
			name: "expected members defaults to one",
			sample: HealthSample{
				Reachable:     true,
				ThresholdsMet: true,
				Members:       0,
				Gaps:          0,
				QueueSize:     0,
			},
			expectedMembers: 0,
			want:            HealthForming,
		},
	}

	for _, tc := range tests {
		tc := tc
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			got := ClassifyHealth(tc.sample, tc.expectedMembers)
			if got != tc.want {
				t.Fatalf("ClassifyHealth() = %s, want %s", got, tc.want)
			}
		})
	}
}
