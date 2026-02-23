package fake

import (
	"context"
	"fmt"

	"ployz/internal/adapter/fake/fault"
	"ployz/internal/check"
	"ployz/internal/deploy"
)

var _ deploy.DeploymentStore = (*ClusterDeploymentStore)(nil)

const (
	FaultClusterDeploymentStoreEnsureTable      = "cluster_deployment_store.ensure_table"
	FaultClusterDeploymentStoreInsert           = "cluster_deployment_store.insert"
	FaultClusterDeploymentStoreUpdate           = "cluster_deployment_store.update"
	FaultClusterDeploymentStoreGet              = "cluster_deployment_store.get"
	FaultClusterDeploymentStoreGetActive        = "cluster_deployment_store.get_active"
	FaultClusterDeploymentStoreListByNamespace  = "cluster_deployment_store.list_by_namespace"
	FaultClusterDeploymentStoreLatestSuccessful = "cluster_deployment_store.latest_successful"
	FaultClusterDeploymentStoreDelete           = "cluster_deployment_store.delete"
	FaultClusterDeploymentStoreAcquireOwnership = "cluster_deployment_store.acquire_ownership"
	FaultClusterDeploymentStoreCheckOwnership   = "cluster_deployment_store.check_ownership"
	FaultClusterDeploymentStoreBumpHeartbeat    = "cluster_deployment_store.bump_ownership_heartbeat"
	FaultClusterDeploymentStoreReleaseOwnership = "cluster_deployment_store.release_ownership"
)

// ClusterDeploymentStore is a deploy.DeploymentStore backed by a cluster node.
type ClusterDeploymentStore struct {
	CallRecorder
	cluster *Cluster
	nodeID  string
	faults  *fault.Injector

	EnsureDeploymentTableErr  func(ctx context.Context) error
	InsertDeploymentErr       func(ctx context.Context, row deploy.DeploymentRow) error
	UpdateDeploymentErr       func(ctx context.Context, row deploy.DeploymentRow) error
	GetDeploymentErr          func(ctx context.Context, id string) error
	GetActiveDeploymentErr    func(ctx context.Context, namespace string) error
	ListByNamespaceErr        func(ctx context.Context, namespace string) error
	LatestSuccessfulErr       func(ctx context.Context, namespace string) error
	DeleteDeploymentErr       func(ctx context.Context, id string) error
	AcquireOwnershipErr       func(ctx context.Context, deployID, machineID, now string) error
	CheckOwnershipErr         func(ctx context.Context, deployID, machineID string) error
	BumpOwnershipHeartbeatErr func(ctx context.Context, deployID, machineID, now string) error
	ReleaseOwnershipErr       func(ctx context.Context, deployID string) error
}

func NewClusterDeploymentStore(cluster *Cluster, nodeID string) *ClusterDeploymentStore {
	check.Assert(cluster != nil, "NewClusterDeploymentStore: cluster must not be nil")
	check.Assert(nodeID != "", "NewClusterDeploymentStore: nodeID must not be empty")
	cluster.Registry(nodeID)
	return &ClusterDeploymentStore{cluster: cluster, nodeID: nodeID, faults: fault.NewInjector()}
}

func (s *ClusterDeploymentStore) FailOnce(point string, err error) {
	s.faults.FailOnce(point, err)
}

func (s *ClusterDeploymentStore) FailAlways(point string, err error) {
	s.faults.FailAlways(point, err)
}

func (s *ClusterDeploymentStore) SetFaultHook(point string, hook fault.Hook) {
	s.faults.SetHook(point, hook)
}

func (s *ClusterDeploymentStore) ClearFault(point string) {
	s.faults.Clear(point)
}

func (s *ClusterDeploymentStore) ResetFaults() {
	s.faults.Reset()
}

func (s *ClusterDeploymentStore) evalFault(point string, args ...any) error {
	check.Assert(s != nil, "ClusterDeploymentStore.evalFault: receiver must not be nil")
	check.Assert(s.faults != nil, "ClusterDeploymentStore.evalFault: faults injector must not be nil")
	if s == nil || s.faults == nil {
		return nil
	}
	return s.faults.Eval(point, args...)
}

func (s *ClusterDeploymentStore) EnsureDeploymentTable(ctx context.Context) error {
	s.record("EnsureDeploymentTable")
	if s.cluster.IsKilled(s.nodeID) {
		return ErrNodeDead
	}
	if err := s.evalFault(FaultClusterDeploymentStoreEnsureTable, ctx); err != nil {
		return err
	}
	if s.EnsureDeploymentTableErr != nil {
		return s.EnsureDeploymentTableErr(ctx)
	}
	return nil
}

func (s *ClusterDeploymentStore) InsertDeployment(ctx context.Context, row deploy.DeploymentRow) error {
	s.record("InsertDeployment", row)
	if s.cluster.IsKilled(s.nodeID) {
		return ErrNodeDead
	}
	if err := s.evalFault(FaultClusterDeploymentStoreInsert, ctx, row); err != nil {
		return err
	}
	if s.InsertDeploymentErr != nil {
		if err := s.InsertDeploymentErr(ctx, row); err != nil {
			return err
		}
	}
	s.cluster.WriteDeployment(s.nodeID, row)
	return nil
}

func (s *ClusterDeploymentStore) UpdateDeployment(ctx context.Context, row deploy.DeploymentRow) error {
	s.record("UpdateDeployment", row)
	if s.cluster.IsKilled(s.nodeID) {
		return ErrNodeDead
	}
	if err := s.evalFault(FaultClusterDeploymentStoreUpdate, ctx, row); err != nil {
		return err
	}
	if s.UpdateDeploymentErr != nil {
		if err := s.UpdateDeploymentErr(ctx, row); err != nil {
			return err
		}
	}

	existing, ok := s.findDeploymentByID(row.ID)
	if !ok {
		return fmt.Errorf("deployment %q not found", row.ID)
	}
	if row.CreatedAt == "" {
		row.CreatedAt = existing.CreatedAt
	}
	s.cluster.WriteDeployment(s.nodeID, row)
	return nil
}

func (s *ClusterDeploymentStore) GetDeployment(ctx context.Context, id string) (deploy.DeploymentRow, bool, error) {
	s.record("GetDeployment", id)
	if s.cluster.IsKilled(s.nodeID) {
		return deploy.DeploymentRow{}, false, ErrNodeDead
	}
	if err := s.evalFault(FaultClusterDeploymentStoreGet, ctx, id); err != nil {
		return deploy.DeploymentRow{}, false, err
	}
	if s.GetDeploymentErr != nil {
		if err := s.GetDeploymentErr(ctx, id); err != nil {
			return deploy.DeploymentRow{}, false, err
		}
	}

	row, ok := s.findDeploymentByID(id)
	if !ok {
		return deploy.DeploymentRow{}, false, nil
	}
	return cloneDeploymentRow(row), true, nil
}

func (s *ClusterDeploymentStore) GetActiveDeployment(ctx context.Context, namespace string) (deploy.DeploymentRow, bool, error) {
	s.record("GetActiveDeployment", namespace)
	if s.cluster.IsKilled(s.nodeID) {
		return deploy.DeploymentRow{}, false, ErrNodeDead
	}
	if err := s.evalFault(FaultClusterDeploymentStoreGetActive, ctx, namespace); err != nil {
		return deploy.DeploymentRow{}, false, err
	}
	if s.GetActiveDeploymentErr != nil {
		if err := s.GetActiveDeploymentErr(ctx, namespace); err != nil {
			return deploy.DeploymentRow{}, false, err
		}
	}

	rows, err := s.ListByNamespace(ctx, namespace)
	if err != nil {
		return deploy.DeploymentRow{}, false, err
	}
	for _, row := range rows {
		if row.Status == "in_progress" {
			return row, true, nil
		}
	}
	return deploy.DeploymentRow{}, false, nil
}

func (s *ClusterDeploymentStore) ListByNamespace(ctx context.Context, namespace string) ([]deploy.DeploymentRow, error) {
	s.record("ListByNamespace", namespace)
	if s.cluster.IsKilled(s.nodeID) {
		return nil, ErrNodeDead
	}
	if err := s.evalFault(FaultClusterDeploymentStoreListByNamespace, ctx, namespace); err != nil {
		return nil, err
	}
	if s.ListByNamespaceErr != nil {
		if err := s.ListByNamespaceErr(ctx, namespace); err != nil {
			return nil, err
		}
	}

	rows := s.cluster.ReadDeployments(s.nodeID)
	out := make([]deploy.DeploymentRow, 0, len(rows))
	for _, row := range rows {
		if row.Namespace == namespace {
			out = append(out, cloneDeploymentRow(row))
		}
	}
	return out, nil
}

func (s *ClusterDeploymentStore) LatestSuccessful(ctx context.Context, namespace string) (deploy.DeploymentRow, bool, error) {
	s.record("LatestSuccessful", namespace)
	if s.cluster.IsKilled(s.nodeID) {
		return deploy.DeploymentRow{}, false, ErrNodeDead
	}
	if err := s.evalFault(FaultClusterDeploymentStoreLatestSuccessful, ctx, namespace); err != nil {
		return deploy.DeploymentRow{}, false, err
	}
	if s.LatestSuccessfulErr != nil {
		if err := s.LatestSuccessfulErr(ctx, namespace); err != nil {
			return deploy.DeploymentRow{}, false, err
		}
	}

	rows, err := s.ListByNamespace(ctx, namespace)
	if err != nil {
		return deploy.DeploymentRow{}, false, err
	}
	for _, row := range rows {
		if row.Status == "succeeded" {
			return row, true, nil
		}
	}
	return deploy.DeploymentRow{}, false, nil
}

func (s *ClusterDeploymentStore) DeleteDeployment(ctx context.Context, id string) error {
	s.record("DeleteDeployment", id)
	if s.cluster.IsKilled(s.nodeID) {
		return ErrNodeDead
	}
	if err := s.evalFault(FaultClusterDeploymentStoreDelete, ctx, id); err != nil {
		return err
	}
	if s.DeleteDeploymentErr != nil {
		if err := s.DeleteDeploymentErr(ctx, id); err != nil {
			return err
		}
	}
	s.cluster.DeleteDeployment(s.nodeID, id)
	return nil
}

func (s *ClusterDeploymentStore) AcquireOwnership(ctx context.Context, deployID, machineID, now string) error {
	s.record("AcquireOwnership", deployID, machineID, now)
	if s.cluster.IsKilled(s.nodeID) {
		return ErrNodeDead
	}
	if err := s.evalFault(FaultClusterDeploymentStoreAcquireOwnership, ctx, deployID, machineID, now); err != nil {
		return err
	}
	if s.AcquireOwnershipErr != nil {
		if err := s.AcquireOwnershipErr(ctx, deployID, machineID, now); err != nil {
			return err
		}
	}

	row, ok := s.findDeploymentByID(deployID)
	if !ok {
		return fmt.Errorf("deployment %q not found", deployID)
	}
	if row.Owner != "" && row.Owner != machineID {
		return fmt.Errorf("deployment %q owned by %s", deployID, row.Owner)
	}
	row.Owner = machineID
	row.OwnerHeartbeat = now
	row.UpdatedAt = now
	s.cluster.WriteDeployment(s.nodeID, row)
	return nil
}

func (s *ClusterDeploymentStore) CheckOwnership(ctx context.Context, deployID, machineID string) error {
	s.record("CheckOwnership", deployID, machineID)
	if s.cluster.IsKilled(s.nodeID) {
		return ErrNodeDead
	}
	if err := s.evalFault(FaultClusterDeploymentStoreCheckOwnership, ctx, deployID, machineID); err != nil {
		return err
	}
	if s.CheckOwnershipErr != nil {
		if err := s.CheckOwnershipErr(ctx, deployID, machineID); err != nil {
			return err
		}
	}

	row, ok := s.findDeploymentByID(deployID)
	if !ok {
		return fmt.Errorf("deployment %q not found", deployID)
	}
	if row.Owner != machineID {
		return fmt.Errorf("deployment %q owned by %s", deployID, row.Owner)
	}
	return nil
}

func (s *ClusterDeploymentStore) BumpOwnershipHeartbeat(ctx context.Context, deployID, machineID, now string) error {
	s.record("BumpOwnershipHeartbeat", deployID, machineID, now)
	if s.cluster.IsKilled(s.nodeID) {
		return ErrNodeDead
	}
	if err := s.evalFault(FaultClusterDeploymentStoreBumpHeartbeat, ctx, deployID, machineID, now); err != nil {
		return err
	}
	if s.BumpOwnershipHeartbeatErr != nil {
		if err := s.BumpOwnershipHeartbeatErr(ctx, deployID, machineID, now); err != nil {
			return err
		}
	}

	row, ok := s.findDeploymentByID(deployID)
	if !ok {
		return fmt.Errorf("deployment %q not found", deployID)
	}
	if row.Owner != machineID {
		return fmt.Errorf("deployment %q owned by %s", deployID, row.Owner)
	}
	row.OwnerHeartbeat = now
	row.UpdatedAt = now
	s.cluster.WriteDeployment(s.nodeID, row)
	return nil
}

func (s *ClusterDeploymentStore) ReleaseOwnership(ctx context.Context, deployID string) error {
	s.record("ReleaseOwnership", deployID)
	if s.cluster.IsKilled(s.nodeID) {
		return ErrNodeDead
	}
	if err := s.evalFault(FaultClusterDeploymentStoreReleaseOwnership, ctx, deployID); err != nil {
		return err
	}
	if s.ReleaseOwnershipErr != nil {
		if err := s.ReleaseOwnershipErr(ctx, deployID); err != nil {
			return err
		}
	}

	row, ok := s.findDeploymentByID(deployID)
	if !ok {
		return nil
	}
	row.Owner = ""
	row.OwnerHeartbeat = ""
	s.cluster.WriteDeployment(s.nodeID, row)
	return nil
}

func (s *ClusterDeploymentStore) findDeploymentByID(id string) (deploy.DeploymentRow, bool) {
	for _, row := range s.cluster.ReadDeployments(s.nodeID) {
		if row.ID == id {
			return row, true
		}
	}
	return deploy.DeploymentRow{}, false
}
