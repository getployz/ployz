package fake

import (
	"context"
	"fmt"

	"ployz/internal/adapter/fake/fault"
	"ployz/internal/check"
	"ployz/internal/deploy"
)

var _ deploy.ContainerStore = (*ClusterContainerStore)(nil)

const (
	FaultClusterContainerStoreEnsureTable       = "cluster_container_store.ensure_table"
	FaultClusterContainerStoreInsert            = "cluster_container_store.insert"
	FaultClusterContainerStoreUpdate            = "cluster_container_store.update"
	FaultClusterContainerStoreListByNamespace   = "cluster_container_store.list_by_namespace"
	FaultClusterContainerStoreListByDeploy      = "cluster_container_store.list_by_deploy"
	FaultClusterContainerStoreDelete            = "cluster_container_store.delete"
	FaultClusterContainerStoreDeleteByNamespace = "cluster_container_store.delete_by_namespace"
)

// ClusterContainerStore is a deploy.ContainerStore backed by a cluster node.
type ClusterContainerStore struct {
	CallRecorder
	cluster *Cluster
	nodeID  string
	faults  *fault.Injector

	EnsureContainerTableErr func(ctx context.Context) error
	InsertContainerErr      func(ctx context.Context, row deploy.ContainerRow) error
	UpdateContainerErr      func(ctx context.Context, row deploy.ContainerRow) error
	ListByNamespaceErr      func(ctx context.Context, namespace string) error
	ListByDeployErr         func(ctx context.Context, namespace, deployID string) error
	DeleteContainerErr      func(ctx context.Context, id string) error
	DeleteByNamespaceErr    func(ctx context.Context, namespace string) error
}

func NewClusterContainerStore(cluster *Cluster, nodeID string) *ClusterContainerStore {
	check.Assert(cluster != nil, "NewClusterContainerStore: cluster must not be nil")
	check.Assert(nodeID != "", "NewClusterContainerStore: nodeID must not be empty")
	cluster.Registry(nodeID)
	return &ClusterContainerStore{cluster: cluster, nodeID: nodeID, faults: fault.NewInjector()}
}

func (s *ClusterContainerStore) FailOnce(point string, err error) {
	s.faults.FailOnce(point, err)
}

func (s *ClusterContainerStore) FailAlways(point string, err error) {
	s.faults.FailAlways(point, err)
}

func (s *ClusterContainerStore) SetFaultHook(point string, hook fault.Hook) {
	s.faults.SetHook(point, hook)
}

func (s *ClusterContainerStore) ClearFault(point string) {
	s.faults.Clear(point)
}

func (s *ClusterContainerStore) ResetFaults() {
	s.faults.Reset()
}

func (s *ClusterContainerStore) evalFault(point string, args ...any) error {
	check.Assert(s != nil, "ClusterContainerStore.evalFault: receiver must not be nil")
	check.Assert(s.faults != nil, "ClusterContainerStore.evalFault: faults injector must not be nil")
	if s == nil || s.faults == nil {
		return nil
	}
	return s.faults.Eval(point, args...)
}

func (s *ClusterContainerStore) EnsureContainerTable(ctx context.Context) error {
	s.record("EnsureContainerTable")
	if s.cluster.IsKilled(s.nodeID) {
		return ErrNodeDead
	}
	if err := s.evalFault(FaultClusterContainerStoreEnsureTable, ctx); err != nil {
		return err
	}
	if s.EnsureContainerTableErr != nil {
		return s.EnsureContainerTableErr(ctx)
	}
	return nil
}

func (s *ClusterContainerStore) InsertContainer(ctx context.Context, row deploy.ContainerRow) error {
	s.record("InsertContainer", row)
	if s.cluster.IsKilled(s.nodeID) {
		return ErrNodeDead
	}
	if err := s.evalFault(FaultClusterContainerStoreInsert, ctx, row); err != nil {
		return err
	}
	if s.InsertContainerErr != nil {
		if err := s.InsertContainerErr(ctx, row); err != nil {
			return err
		}
	}
	s.cluster.WriteContainer(s.nodeID, row)
	return nil
}

func (s *ClusterContainerStore) UpdateContainer(ctx context.Context, row deploy.ContainerRow) error {
	s.record("UpdateContainer", row)
	if s.cluster.IsKilled(s.nodeID) {
		return ErrNodeDead
	}
	if err := s.evalFault(FaultClusterContainerStoreUpdate, ctx, row); err != nil {
		return err
	}
	if s.UpdateContainerErr != nil {
		if err := s.UpdateContainerErr(ctx, row); err != nil {
			return err
		}
	}

	rows := s.cluster.ReadContainers(s.nodeID)
	for _, existing := range rows {
		if existing.ID != row.ID {
			continue
		}
		if row.CreatedAt == "" {
			row.CreatedAt = existing.CreatedAt
		}
		s.cluster.WriteContainer(s.nodeID, row)
		return nil
	}
	return fmt.Errorf("container %q not found", row.ID)
}

func (s *ClusterContainerStore) ListContainersByNamespace(ctx context.Context, namespace string) ([]deploy.ContainerRow, error) {
	s.record("ListContainersByNamespace", namespace)
	if s.cluster.IsKilled(s.nodeID) {
		return nil, ErrNodeDead
	}
	if err := s.evalFault(FaultClusterContainerStoreListByNamespace, ctx, namespace); err != nil {
		return nil, err
	}
	if s.ListByNamespaceErr != nil {
		if err := s.ListByNamespaceErr(ctx, namespace); err != nil {
			return nil, err
		}
	}
	return s.cluster.ReadContainersByNamespace(s.nodeID, namespace), nil
}

func (s *ClusterContainerStore) ListContainersByDeploy(ctx context.Context, namespace, deployID string) ([]deploy.ContainerRow, error) {
	s.record("ListContainersByDeploy", namespace, deployID)
	if s.cluster.IsKilled(s.nodeID) {
		return nil, ErrNodeDead
	}
	if err := s.evalFault(FaultClusterContainerStoreListByDeploy, ctx, namespace, deployID); err != nil {
		return nil, err
	}
	if s.ListByDeployErr != nil {
		if err := s.ListByDeployErr(ctx, namespace, deployID); err != nil {
			return nil, err
		}
	}

	rows := s.cluster.ReadContainersByNamespace(s.nodeID, namespace)
	out := make([]deploy.ContainerRow, 0, len(rows))
	for _, row := range rows {
		if row.DeployID == deployID {
			out = append(out, row)
		}
	}
	return out, nil
}

func (s *ClusterContainerStore) DeleteContainer(ctx context.Context, id string) error {
	s.record("DeleteContainer", id)
	if s.cluster.IsKilled(s.nodeID) {
		return ErrNodeDead
	}
	if err := s.evalFault(FaultClusterContainerStoreDelete, ctx, id); err != nil {
		return err
	}
	if s.DeleteContainerErr != nil {
		if err := s.DeleteContainerErr(ctx, id); err != nil {
			return err
		}
	}
	s.cluster.DeleteContainer(s.nodeID, id)
	return nil
}

func (s *ClusterContainerStore) DeleteContainersByNamespace(ctx context.Context, namespace string) error {
	s.record("DeleteContainersByNamespace", namespace)
	if s.cluster.IsKilled(s.nodeID) {
		return ErrNodeDead
	}
	if err := s.evalFault(FaultClusterContainerStoreDeleteByNamespace, ctx, namespace); err != nil {
		return err
	}
	if s.DeleteByNamespaceErr != nil {
		if err := s.DeleteByNamespaceErr(ctx, namespace); err != nil {
			return err
		}
	}
	rows := s.cluster.ReadContainersByNamespace(s.nodeID, namespace)
	for _, row := range rows {
		s.cluster.DeleteContainer(s.nodeID, row.ID)
	}
	return nil
}
