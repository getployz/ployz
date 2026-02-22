//go:build !debug

package check

// Assert is a no-op in release builds.
func Assert(_ bool, _ string) {}

// Assertf is a no-op in release builds.
func Assertf(_ bool, _ string, _ ...any) {}
