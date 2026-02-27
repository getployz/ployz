package convergence

type Topic uint8

const (
	TopicMachines Topic = iota + 1
	TopicHeartbeats
	TopicContainers
	TopicDeployments
)
