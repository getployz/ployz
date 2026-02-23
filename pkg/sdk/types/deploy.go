package types

import "time"

type DeployPlan struct {
	Namespace string       `json:"namespace"`
	DeployID  string       `json:"deploy_id"`
	Tiers     []DeployTier `json:"tiers"`
}

type DeployTier struct {
	Services []DeployServicePlan `json:"services"`
}

type DeployServicePlan struct {
	Name            string             `json:"name"`
	UpToDate        []DeployPlanEntry  `json:"up_to_date,omitempty"`
	NeedsSpecUpdate []DeployPlanEntry  `json:"needs_spec_update,omitempty"`
	NeedsUpdate     []DeployPlanEntry  `json:"needs_update,omitempty"`
	NeedsRecreate   []DeployPlanEntry  `json:"needs_recreate,omitempty"`
	Create          []DeployPlanEntry  `json:"create,omitempty"`
	Remove          []DeployPlanEntry  `json:"remove,omitempty"`
	UpdateConfig    DeployUpdateConfig `json:"update_config"`
	HealthCheck     *DeployHealthCheck `json:"health_check,omitempty"`
}

type DeployPlanEntry struct {
	MachineID      string `json:"machine_id"`
	ContainerName  string `json:"container_name"`
	SpecJSON       string `json:"spec_json"`
	CurrentRowJSON string `json:"current_row_json,omitempty"`
	Reason         string `json:"reason,omitempty"`
}

type DeployUpdateConfig struct {
	Order         string `json:"order,omitempty"`
	Parallelism   int    `json:"parallelism,omitempty"`
	FailureAction string `json:"failure_action,omitempty"`
}

type DeployHealthCheck struct {
	Test        []string      `json:"test,omitempty"`
	Interval    time.Duration `json:"interval,omitempty"`
	Timeout     time.Duration `json:"timeout,omitempty"`
	Retries     int           `json:"retries,omitempty"`
	StartPeriod time.Duration `json:"start_period,omitempty"`
}

type DeployResult struct {
	Namespace    string             `json:"namespace"`
	DeployID     string             `json:"deploy_id"`
	Status       string             `json:"status"`
	Tiers        []DeployTierResult `json:"tiers,omitempty"`
	ErrorMessage string             `json:"error_message,omitempty"`
	ErrorPhase   string             `json:"error_phase,omitempty"`
	ErrorTier    int                `json:"error_tier,omitempty"`
}

type DeployTierResult struct {
	Name       string                  `json:"name"`
	Status     string                  `json:"status"`
	Containers []DeployContainerResult `json:"containers,omitempty"`
}

type DeployContainerResult struct {
	MachineID     string `json:"machine_id"`
	ContainerName string `json:"container_name"`
	Expected      string `json:"expected,omitempty"`
	Actual        string `json:"actual,omitempty"`
	Match         bool   `json:"match"`
}

type DeployProgressEvent struct {
	Type      string `json:"type"`
	Tier      int    `json:"tier"`
	Service   string `json:"service,omitempty"`
	MachineID string `json:"machine_id,omitempty"`
	Container string `json:"container,omitempty"`
	Message   string `json:"message,omitempty"`
}

type DeploymentEntry struct {
	ID             string            `json:"id"`
	Namespace      string            `json:"namespace"`
	Status         string            `json:"status"`
	Owner          string            `json:"owner,omitempty"`
	OwnerHeartbeat string            `json:"owner_heartbeat,omitempty"`
	Labels         map[string]string `json:"labels,omitempty"`
	MachineIDs     []string          `json:"machine_ids,omitempty"`
	Version        int64             `json:"version"`
	CreatedAt      string            `json:"created_at,omitempty"`
	UpdatedAt      string            `json:"updated_at,omitempty"`
}

type ContainerState struct {
	ContainerName string `json:"container_name"`
	Image         string `json:"image"`
	Running       bool   `json:"running"`
	Healthy       bool   `json:"healthy"`
}
