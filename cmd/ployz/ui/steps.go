package ui

type stepStatus string

const (
	stepPending stepStatus = "pending"
	stepRunning stepStatus = "running"
	stepDone    stepStatus = "done"
	stepFailed  stepStatus = "failed"
)

type stepState struct {
	ID       string
	ParentID string
	Title    string
	Status   stepStatus
	Message  string

	synthetic bool
}

type stepSnapshot struct {
	Steps []stepState
}
