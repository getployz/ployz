//go:build debug

package check

import "fmt"

// Assert panics if cond is false. Only active in debug builds.
func Assert(cond bool, msg string) {
	if !cond {
		panic("assertion failed: " + msg)
	}
}

// Assertf panics if cond is false with a formatted message. Only active in debug builds.
func Assertf(cond bool, format string, args ...any) {
	if !cond {
		panic("assertion failed: " + fmt.Sprintf(format, args...))
	}
}
