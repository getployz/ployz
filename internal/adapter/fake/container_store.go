package fake

import (
	"context"
	"fmt"
	"sort"
	"sync"

	"ployz/internal/adapter/fake/fault"
	"ployz/internal/check"
	"ployz/internal/deploy"
)

var _ deploy.ContainerStore = (*ContainerStore)(nil)

const (
	FaultContainerStoreEnsureTable       = "container_store.ensure_table"
	FaultContainerStoreInsert            = "container_store.insert"
	FaultContainerStoreUpdate            = "container_store.update"
	FaultContainerStoreListByNamespace   = "container_store.list_by_namespace"
	FaultContainerStoreListByDeploy      = "container_store.list_by_deploy"
	FaultContainerStoreDelete            = "container_store.delete"
	FaultContainerStoreDeleteByNamespace = "container_store.delete_by_namespace"
)

// ContainerStore is an in-memory implementation of deploy.ContainerStore.
type ContainerStore struct {
	CallRecorder
	mu         sync.Mutex
	containers map[string]deploy.ContainerRow
	faults     *fault.Injector

	EnsureContainerTableErr func(ctx context.Context) error
	InsertContainerErr      func(ctx context.Context, row deploy.ContainerRow) error
	UpdateContainerErr      func(ctx context.Context, row deploy.ContainerRow) error
	ListByNamespaceErr      func(ctx context.Context, namespace string) error
	ListByDeployErr         func(ctx context.Context, namespace, deployID string) error
	DeleteContainerErr      func(ctx context.Context, id string) error
	DeleteByNamespaceErr    func(ctx context.Context, namespace string) error
}

func NewContainerStore() *ContainerStore {
	return &ContainerStore{containers: make(map[string]deploy.ContainerRow), faults: fault.NewInjector()}
}

func (s *ContainerStore) FailOnce(point string, err error) {
	s.faults.FailOnce(point, err)
}

func (s *ContainerStore) FailAlways(point string, err error) {
	s.faults.FailAlways(point, err)
}

func (s *ContainerStore) SetFaultHook(point string, hook fault.Hook) {
	s.faults.SetHook(point, hook)
}

func (s *ContainerStore) ClearFault(point string) {
	s.faults.Clear(point)
}

func (s *ContainerStore) ResetFaults() {
	s.faults.Reset()
}

func (s *ContainerStore) evalFault(point string, args ...any) error {
	check.Assert(s != nil, "ContainerStore.evalFault: receiver must not be nil")
	check.Assert(s.faults != nil, "ContainerStore.evalFault: faults injector must not be nil")
	if s == nil || s.faults == nil {
		return nil
	}
	return s.faults.Eval(point, args...)
}

func (s *ContainerStore) EnsureContainerTable(ctx context.Context) error {
	s.record("EnsureContainerTable")
	if err := s.evalFault(FaultContainerStoreEnsureTable, ctx); err != nil {
		return err
	}
	if s.EnsureContainerTableErr != nil {
		return s.EnsureContainerTableErr(ctx)
	}
	return nil
}

func (s *ContainerStore) InsertContainer(ctx context.Context, row deploy.ContainerRow) error {
	s.record("InsertContainer", row)
	if err := s.evalFault(FaultContainerStoreInsert, ctx, row); err != nil {
		return err
	}
	if s.InsertContainerErr != nil {
		if err := s.InsertContainerErr(ctx, row); err != nil {
			return err
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()

	if row.Version <= 0 {
		row.Version = 1
	}
	s.containers[row.ID] = row
	return nil
}

func (s *ContainerStore) UpdateContainer(ctx context.Context, row deploy.ContainerRow) error {
	s.record("UpdateContainer", row)
	if err := s.evalFault(FaultContainerStoreUpdate, ctx, row); err != nil {
		return err
	}
	if s.UpdateContainerErr != nil {
		if err := s.UpdateContainerErr(ctx, row); err != nil {
			return err
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()

	existing, ok := s.containers[row.ID]
	if !ok {
		return fmt.Errorf("container %q not found", row.ID)
	}
	row.Version = existing.Version + 1
	if row.CreatedAt == "" {
		row.CreatedAt = existing.CreatedAt
	}
	s.containers[row.ID] = row
	return nil
}

func (s *ContainerStore) ListContainersByNamespace(ctx context.Context, namespace string) ([]deploy.ContainerRow, error) {
	s.record("ListContainersByNamespace", namespace)
	if err := s.evalFault(FaultContainerStoreListByNamespace, ctx, namespace); err != nil {
		return nil, err
	}
	if s.ListByNamespaceErr != nil {
		if err := s.ListByNamespaceErr(ctx, namespace); err != nil {
			return nil, err
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()

	out := make([]deploy.ContainerRow, 0, len(s.containers))
	for _, row := range s.containers {
		if row.Namespace == namespace {
			out = append(out, row)
		}
	}
	sortContainerRows(out)
	return out, nil
}

func (s *ContainerStore) ListContainersByDeploy(ctx context.Context, namespace, deployID string) ([]deploy.ContainerRow, error) {
	s.record("ListContainersByDeploy", namespace, deployID)
	if err := s.evalFault(FaultContainerStoreListByDeploy, ctx, namespace, deployID); err != nil {
		return nil, err
	}
	if s.ListByDeployErr != nil {
		if err := s.ListByDeployErr(ctx, namespace, deployID); err != nil {
			return nil, err
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()

	out := make([]deploy.ContainerRow, 0, len(s.containers))
	for _, row := range s.containers {
		if row.Namespace == namespace && row.DeployID == deployID {
			out = append(out, row)
		}
	}
	sortContainerRows(out)
	return out, nil
}

func (s *ContainerStore) DeleteContainer(ctx context.Context, id string) error {
	s.record("DeleteContainer", id)
	if err := s.evalFault(FaultContainerStoreDelete, ctx, id); err != nil {
		return err
	}
	if s.DeleteContainerErr != nil {
		if err := s.DeleteContainerErr(ctx, id); err != nil {
			return err
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()

	delete(s.containers, id)
	return nil
}

func (s *ContainerStore) DeleteContainersByNamespace(ctx context.Context, namespace string) error {
	s.record("DeleteContainersByNamespace", namespace)
	if err := s.evalFault(FaultContainerStoreDeleteByNamespace, ctx, namespace); err != nil {
		return err
	}
	if s.DeleteByNamespaceErr != nil {
		if err := s.DeleteByNamespaceErr(ctx, namespace); err != nil {
			return err
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()

	for id, row := range s.containers {
		if row.Namespace == namespace {
			delete(s.containers, id)
		}
	}
	return nil
}

func sortContainerRows(rows []deploy.ContainerRow) {
	sort.Slice(rows, func(i, j int) bool {
		if rows[i].Service != rows[j].Service {
			return rows[i].Service < rows[j].Service
		}
		if rows[i].MachineID != rows[j].MachineID {
			return rows[i].MachineID < rows[j].MachineID
		}
		return rows[i].ContainerName < rows[j].ContainerName
	})
}
