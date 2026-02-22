package fake

import "sync"

// Call records a single method invocation.
type Call struct {
	Method string
	Args   []any
}

// CallRecorder tracks method calls for assertion in tests.
type CallRecorder struct {
	mu    sync.Mutex
	calls []Call
}

func (r *CallRecorder) record(method string, args ...any) {
	r.mu.Lock()
	r.calls = append(r.calls, Call{Method: method, Args: args})
	r.mu.Unlock()
}

// Calls returns recorded calls. If method is "", returns all calls.
func (r *CallRecorder) Calls(method string) []Call {
	r.mu.Lock()
	defer r.mu.Unlock()

	if method == "" {
		out := make([]Call, len(r.calls))
		copy(out, r.calls)
		return out
	}

	var out []Call
	for _, c := range r.calls {
		if c.Method == method {
			out = append(out, c)
		}
	}
	return out
}

// Reset clears all recorded calls.
func (r *CallRecorder) Reset() {
	r.mu.Lock()
	r.calls = nil
	r.mu.Unlock()
}
