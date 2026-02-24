package deploy

import (
	"context"
	"fmt"

	"ployz/internal/check"
	"ployz/internal/network"
)

// RemoveNamespace removes all local containers for a namespace and deletes
// namespace container rows from the store.
func RemoveNamespace(
	ctx context.Context,
	rt network.ContainerRuntime,
	stores Stores,
	namespace string,
	machineID string,
) error {
	check.Assert(rt != nil, "RemoveNamespace: container runtime must not be nil")
	check.Assert(stores.Containers != nil, "RemoveNamespace: container store must not be nil")

	_ = machineID

	containers, err := rt.ContainerList(ctx, map[string]string{labelNamespace: namespace})
	if err != nil {
		return fmt.Errorf("list namespace containers %q: %w", namespace, err)
	}

	var firstErr error
	for _, container := range containers {
		if stopErr := rt.ContainerStop(ctx, container.Name); stopErr != nil && firstErr == nil {
			firstErr = fmt.Errorf("stop container %s: %w", container.Name, stopErr)
		}
		if removeErr := rt.ContainerRemove(ctx, container.Name, true); removeErr != nil && firstErr == nil {
			firstErr = fmt.Errorf("remove container %s: %w", container.Name, removeErr)
		}
	}

	if err := stores.Containers.DeleteContainersByNamespace(ctx, namespace); err != nil {
		if firstErr != nil {
			return fmt.Errorf("%w; delete namespace rows %q: %v", firstErr, namespace, err)
		}
		return fmt.Errorf("delete namespace rows %q: %w", namespace, err)
	}

	return firstErr
}
