package ui

import "testing"

func TestEnvTruthyValues(t *testing.T) {
	testCases := []struct {
		name  string
		value string
		want  bool
	}{
		{name: "one", value: "1", want: true},
		{name: "true", value: "true", want: true},
		{name: "yes", value: "yes", want: true},
		{name: "on", value: "on", want: true},
		{name: "zero", value: "0", want: false},
		{name: "false", value: "false", want: false},
		{name: "empty", value: "", want: false},
	}

	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			t.Setenv("PLOYZ_TEST_TRUTHY", tc.value)
			if got := envTruthy("PLOYZ_TEST_TRUTHY"); got != tc.want {
				t.Fatalf("envTruthy() = %v, want %v", got, tc.want)
			}
		})
	}
}
