package types

type PreconditionCode string

const (
	PreconditionNetworkNotConfigured       PreconditionCode = "network.not_configured"
	PreconditionRuntimeNotReadyForServices PreconditionCode = "runtime.not_ready_for_services"
	PreconditionDeployNoMachinesAvailable  PreconditionCode = "deploy.no_machines_available"
	PreconditionNetworkDestroyHasWorkloads PreconditionCode = "network.destroy_has_workloads"
	PreconditionNetworkDestroyHasMachines  PreconditionCode = "network.destroy_has_machines"
)
