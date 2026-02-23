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

var _ deploy.DeploymentStore = (*DeploymentStore)(nil)

const (
	FaultDeploymentStoreEnsureTable      = "deployment_store.ensure_table"
	FaultDeploymentStoreInsert           = "deployment_store.insert"
	FaultDeploymentStoreUpdate           = "deployment_store.update"
	FaultDeploymentStoreGet              = "deployment_store.get"
	FaultDeploymentStoreGetActive        = "deployment_store.get_active"
	FaultDeploymentStoreListByNamespace  = "deployment_store.list_by_namespace"
	FaultDeploymentStoreLatestSuccessful = "deployment_store.latest_successful"
	FaultDeploymentStoreDelete           = "deployment_store.delete"
	FaultDeploymentStoreAcquireOwnership = "deployment_store.acquire_ownership"
	FaultDeploymentStoreCheckOwnership   = "deployment_store.check_ownership"
	FaultDeploymentStoreBumpHeartbeat    = "deployment_store.bump_ownership_heartbeat"
	FaultDeploymentStoreReleaseOwnership = "deployment_store.release_ownership"
)

// DeploymentStore is an in-memory implementation of deploy.DeploymentStore.
type DeploymentStore struct {
	CallRecorder
	mu          sync.Mutex
	deployments map[string]deploy.DeploymentRow
	faults      *fault.Injector

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

func NewDeploymentStore() *DeploymentStore {
	return &DeploymentStore{deployments: make(map[string]deploy.DeploymentRow), faults: fault.NewInjector()}
}

func (s *DeploymentStore) FailOnce(point string, err error) {
	s.faults.FailOnce(point, err)
}

func (s *DeploymentStore) FailAlways(point string, err error) {
	s.faults.FailAlways(point, err)
}

func (s *DeploymentStore) SetFaultHook(point string, hook fault.Hook) {
	s.faults.SetHook(point, hook)
}

func (s *DeploymentStore) ClearFault(point string) {
	s.faults.Clear(point)
}

func (s *DeploymentStore) ResetFaults() {
	s.faults.Reset()
}

func (s *DeploymentStore) evalFault(point string, args ...any) error {
	check.Assert(s != nil, "DeploymentStore.evalFault: receiver must not be nil")
	check.Assert(s.faults != nil, "DeploymentStore.evalFault: faults injector must not be nil")
	if s == nil || s.faults == nil {
		return nil
	}
	return s.faults.Eval(point, args...)
}

func (s *DeploymentStore) EnsureDeploymentTable(ctx context.Context) error {
	s.record("EnsureDeploymentTable")
	if err := s.evalFault(FaultDeploymentStoreEnsureTable, ctx); err != nil {
		return err
	}
	if s.EnsureDeploymentTableErr != nil {
		return s.EnsureDeploymentTableErr(ctx)
	}
	return nil
}

func (s *DeploymentStore) InsertDeployment(ctx context.Context, row deploy.DeploymentRow) error {
	s.record("InsertDeployment", row)
	if err := s.evalFault(FaultDeploymentStoreInsert, ctx, row); err != nil {
		return err
	}
	if s.InsertDeploymentErr != nil {
		if err := s.InsertDeploymentErr(ctx, row); err != nil {
			return err
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()

	if row.Version <= 0 {
		row.Version = 1
	}
	s.deployments[row.ID] = cloneDeploymentRow(row)
	return nil
}

func (s *DeploymentStore) UpdateDeployment(ctx context.Context, row deploy.DeploymentRow) error {
	s.record("UpdateDeployment", row)
	if err := s.evalFault(FaultDeploymentStoreUpdate, ctx, row); err != nil {
		return err
	}
	if s.UpdateDeploymentErr != nil {
		if err := s.UpdateDeploymentErr(ctx, row); err != nil {
			return err
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()

	existing, ok := s.deployments[row.ID]
	if !ok {
		return fmt.Errorf("deployment %q not found", row.ID)
	}
	row.Version = existing.Version + 1
	if row.CreatedAt == "" {
		row.CreatedAt = existing.CreatedAt
	}
	s.deployments[row.ID] = cloneDeploymentRow(row)
	return nil
}

func (s *DeploymentStore) GetDeployment(ctx context.Context, id string) (deploy.DeploymentRow, bool, error) {
	s.record("GetDeployment", id)
	if err := s.evalFault(FaultDeploymentStoreGet, ctx, id); err != nil {
		return deploy.DeploymentRow{}, false, err
	}
	if s.GetDeploymentErr != nil {
		if err := s.GetDeploymentErr(ctx, id); err != nil {
			return deploy.DeploymentRow{}, false, err
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()

	row, ok := s.deployments[id]
	if !ok {
		return deploy.DeploymentRow{}, false, nil
	}
	return cloneDeploymentRow(row), true, nil
}

func (s *DeploymentStore) GetActiveDeployment(ctx context.Context, namespace string) (deploy.DeploymentRow, bool, error) {
	s.record("GetActiveDeployment", namespace)
	if err := s.evalFault(FaultDeploymentStoreGetActive, ctx, namespace); err != nil {
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

func (s *DeploymentStore) ListByNamespace(ctx context.Context, namespace string) ([]deploy.DeploymentRow, error) {
	s.record("ListByNamespace", namespace)
	if err := s.evalFault(FaultDeploymentStoreListByNamespace, ctx, namespace); err != nil {
		return nil, err
	}
	if s.ListByNamespaceErr != nil {
		if err := s.ListByNamespaceErr(ctx, namespace); err != nil {
			return nil, err
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()

	out := make([]deploy.DeploymentRow, 0, len(s.deployments))
	for _, row := range s.deployments {
		if row.Namespace == namespace {
			out = append(out, cloneDeploymentRow(row))
		}
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].CreatedAt != out[j].CreatedAt {
			return out[i].CreatedAt > out[j].CreatedAt
		}
		return out[i].ID > out[j].ID
	})
	return out, nil
}

func (s *DeploymentStore) LatestSuccessful(ctx context.Context, namespace string) (deploy.DeploymentRow, bool, error) {
	s.record("LatestSuccessful", namespace)
	if err := s.evalFault(FaultDeploymentStoreLatestSuccessful, ctx, namespace); err != nil {
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

func (s *DeploymentStore) DeleteDeployment(ctx context.Context, id string) error {
	s.record("DeleteDeployment", id)
	if err := s.evalFault(FaultDeploymentStoreDelete, ctx, id); err != nil {
		return err
	}
	if s.DeleteDeploymentErr != nil {
		if err := s.DeleteDeploymentErr(ctx, id); err != nil {
			return err
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()

	delete(s.deployments, id)
	return nil
}

func (s *DeploymentStore) AcquireOwnership(ctx context.Context, deployID, machineID, now string) error {
	s.record("AcquireOwnership", deployID, machineID, now)
	if err := s.evalFault(FaultDeploymentStoreAcquireOwnership, ctx, deployID, machineID, now); err != nil {
		return err
	}
	if s.AcquireOwnershipErr != nil {
		if err := s.AcquireOwnershipErr(ctx, deployID, machineID, now); err != nil {
			return err
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()

	row, ok := s.deployments[deployID]
	if !ok {
		return fmt.Errorf("deployment %q not found", deployID)
	}
	if row.Owner != "" && row.Owner != machineID {
		return fmt.Errorf("deployment %q owned by %s", deployID, row.Owner)
	}
	row.Owner = machineID
	row.OwnerHeartbeat = now
	row.UpdatedAt = now
	row.Version++
	s.deployments[deployID] = cloneDeploymentRow(row)
	return nil
}

func (s *DeploymentStore) CheckOwnership(ctx context.Context, deployID, machineID string) error {
	s.record("CheckOwnership", deployID, machineID)
	if err := s.evalFault(FaultDeploymentStoreCheckOwnership, ctx, deployID, machineID); err != nil {
		return err
	}
	if s.CheckOwnershipErr != nil {
		if err := s.CheckOwnershipErr(ctx, deployID, machineID); err != nil {
			return err
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()

	row, ok := s.deployments[deployID]
	if !ok {
		return fmt.Errorf("deployment %q not found", deployID)
	}
	if row.Owner != machineID {
		return fmt.Errorf("deployment %q owned by %s", deployID, row.Owner)
	}
	return nil
}

func (s *DeploymentStore) BumpOwnershipHeartbeat(ctx context.Context, deployID, machineID, now string) error {
	s.record("BumpOwnershipHeartbeat", deployID, machineID, now)
	if err := s.evalFault(FaultDeploymentStoreBumpHeartbeat, ctx, deployID, machineID, now); err != nil {
		return err
	}
	if s.BumpOwnershipHeartbeatErr != nil {
		if err := s.BumpOwnershipHeartbeatErr(ctx, deployID, machineID, now); err != nil {
			return err
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()

	row, ok := s.deployments[deployID]
	if !ok {
		return fmt.Errorf("deployment %q not found", deployID)
	}
	if row.Owner != machineID {
		return fmt.Errorf("deployment %q owned by %s", deployID, row.Owner)
	}
	row.OwnerHeartbeat = now
	row.UpdatedAt = now
	row.Version++
	s.deployments[deployID] = cloneDeploymentRow(row)
	return nil
}

func (s *DeploymentStore) ReleaseOwnership(ctx context.Context, deployID string) error {
	s.record("ReleaseOwnership", deployID)
	if err := s.evalFault(FaultDeploymentStoreReleaseOwnership, ctx, deployID); err != nil {
		return err
	}
	if s.ReleaseOwnershipErr != nil {
		if err := s.ReleaseOwnershipErr(ctx, deployID); err != nil {
			return err
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()

	row, ok := s.deployments[deployID]
	if !ok {
		return nil
	}
	row.Owner = ""
	row.OwnerHeartbeat = ""
	row.Version++
	s.deployments[deployID] = cloneDeploymentRow(row)
	return nil
}

func cloneDeploymentRow(in deploy.DeploymentRow) deploy.DeploymentRow {
	out := in
	if in.Labels != nil {
		out.Labels = make(map[string]string, len(in.Labels))
		for key, value := range in.Labels {
			out.Labels[key] = value
		}
	} else {
		out.Labels = map[string]string{}
	}
	out.MachineIDs = append([]string(nil), in.MachineIDs...)
	if out.MachineIDs == nil {
		out.MachineIDs = []string{}
	}
	return out
}
