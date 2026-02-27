package daemon

import "context"

// Run wires and starts the daemon API server.
func Run(ctx context.Context, dataRoot, socketPath string) error {
	srv, err := Wire(ctx, dataRoot)
	if err != nil {
		return err
	}
	return srv.ListenAndServe(ctx, socketPath)
}
