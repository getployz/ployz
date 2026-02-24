package network

import "errors"

// ErrNotInitialized indicates a network has not been set up yet.
var ErrNotInitialized = errors.New("network is not initialized")

// ValidationError indicates an invalid input to a network operation.
type ValidationError struct {
	Field   string
	Message string
}

func (e *ValidationError) Error() string {
	if e.Field != "" {
		return e.Field + ": " + e.Message
	}
	return e.Message
}
