package main

import "testing"

func TestExceptionCanMatchDescendant(t *testing.T) {
	for _, test := range []struct {
		pattern, directory string
		want               bool
	}{
		{"src/*.rs", "node_modules", false},
		{"src/foo*.rs", "src/bar", false},
		{"src/*.rs", "src", true},
		{"src/foo*.rs", "src/foo", true},
		{"**/keep", "node_modules", true},
		{"cache/keep", "cache", true},
		{"cache/keep", "cache-other", false},
		{"cache", "cache", false},
		{`src/\[literal\]/keep`, "src/[literal]", true},
	} {
		t.Run(test.pattern+":"+test.directory, func(t *testing.T) {
			if got := exceptionCanMatchDescendant(test.pattern, test.directory); got != test.want {
				t.Fatalf("got %v, want %v", got, test.want)
			}
		})
	}
}
