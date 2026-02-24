package deploy

import (
	"fmt"
	"time"
)

// PlacementMode controls how a service is distributed across machines.
type PlacementMode string

const (
	PlacementGlobal     PlacementMode = "global"
	PlacementReplicated PlacementMode = "replicated"
)

// ServiceSpec is a normalized, JSON-serializable representation of one compose
// service.
type ServiceSpec struct {
	Name          string            `json:"name"`
	Image         string            `json:"image"`
	Command       []string          `json:"command,omitempty"`
	Entrypoint    []string          `json:"entrypoint,omitempty"`
	Environment   []string          `json:"environment,omitempty"`
	Mounts        []Mount           `json:"mounts,omitempty"`
	Ports         []PortMapping     `json:"ports,omitempty"`
	Labels        map[string]string `json:"labels,omitempty"`
	RestartPolicy string            `json:"restart_policy,omitempty"`
	HealthCheck   *HealthCheck      `json:"health_check,omitempty"`
	Resources     *Resources        `json:"resources,omitempty"`
}

type Mount struct {
	Source   string `json:"source"`
	Target   string `json:"target"`
	ReadOnly bool   `json:"read_only,omitempty"`
}

type PortMapping struct {
	HostPort      uint16 `json:"host_port"`
	ContainerPort uint16 `json:"container_port"`
	Protocol      string `json:"protocol"`
}

type HealthCheck struct {
	Test        []string      `json:"test"`
	Interval    time.Duration `json:"interval"`
	Timeout     time.Duration `json:"timeout"`
	Retries     int           `json:"retries"`
	StartPeriod time.Duration `json:"start_period"`
}

type Resources struct {
	CPULimit    float64 `json:"cpu_limit,omitempty"`
	MemoryLimit int64   `json:"memory_limit,omitempty"`
}

// DeploySpec is the full deployment specification for one namespace.
type DeploySpec struct {
	Namespace string
	Network   string
	Services  []ServiceDeployConfig
}

// ServiceDeployConfig wraps a ServiceSpec with orchestration metadata.
type ServiceDeployConfig struct {
	Spec         ServiceSpec
	Placement    PlacementMode
	Replicas     int
	Constraints  []string
	DeployLabels map[string]string
	UpdateConfig UpdateConfig
	DependsOn    []string
}

type UpdateConfig struct {
	Order         string
	Parallelism   int
	FailureAction string
}

// DeploymentRow maps to the Corrosion deployments table.
type DeploymentRow struct {
	ID             string            `json:"id"`
	Namespace      string            `json:"namespace"`
	SpecJSON       string            `json:"spec_json"`
	Labels         map[string]string `json:"labels"`
	Status         DeployPhase       `json:"status"`
	Owner          string            `json:"owner"`
	OwnerHeartbeat string            `json:"owner_heartbeat"`
	MachineIDs     []string          `json:"machine_ids"`
	Version        int64             `json:"version"`
	CreatedAt      string            `json:"created_at"`
	UpdatedAt      string            `json:"updated_at"`
}

// ContainerRow maps to the Corrosion containers table.
type ContainerRow struct {
	ID            string `json:"id"`
	Namespace     string `json:"namespace"`
	DeployID      string `json:"deploy_id"`
	Service       string `json:"service"`
	MachineID     string `json:"machine_id"`
	ContainerName string `json:"container_name"`
	SpecJSON      string `json:"spec_json"`
	Status        string `json:"status"`
	Version       int64  `json:"version"`
	CreatedAt     string `json:"created_at"`
	UpdatedAt     string `json:"updated_at"`
}

type ChangeKind int

const (
	UpToDate ChangeKind = iota
	NeedsSpecUpdate
	NeedsUpdate
	NeedsRecreate
	Create
	Remove
)

type DeployPlan struct {
	Namespace string
	DeployID  string
	Tiers     []Tier
}

type Tier struct {
	Services []ServicePlan
}

type ServicePlan struct {
	Name            string
	UpToDate        []PlanEntry
	NeedsSpecUpdate []PlanEntry
	NeedsUpdate     []PlanEntry
	NeedsRecreate   []PlanEntry
	Create          []PlanEntry
	Remove          []PlanEntry
	UpdateConfig    UpdateConfig
	HealthCheck     *HealthCheck
}

type PlanEntry struct {
	MachineID     string
	ContainerName string
	Spec          ServiceSpec
	CurrentRow    *ContainerRow
	Reason        string
}

type MachineAssignment struct {
	MachineID     string
	ContainerName string
}

// MachineInfo is the scheduler's view of a machine.
type MachineInfo struct {
	ID     string
	Labels map[string]string
}

type ApplyResult struct {
	Namespace string
	DeployID  string
	Tiers     []TierResult
}

type TierResult struct {
	Name       string
	Status     TierPhase
	Containers []ContainerResult
}

type ContainerResult struct {
	MachineID     string
	ContainerName string
	Expected      string
	Actual        string
	Match         bool
}

// DeployError carries structured context for deploy failures.
type DeployError struct {
	Namespace string
	Phase     DeployErrorPhase
	Tier      int
	TierName  string
	Tiers     []TierResult
	Message   string
}

func (e *DeployError) Error() string {
	if e == nil {
		return "<nil>"
	}
	return fmt.Sprintf("deploy %q failed at %s (tier %d %q): %s", e.Namespace, e.Phase, e.Tier, e.TierName, e.Message)
}

type ProgressEvent struct {
	Type      string
	Tier      int
	Service   string
	MachineID string
	Container string
	Message   string
}
