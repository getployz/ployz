package deploy

import "fmt"

// AssertTierState compares actual machine state to expected tier state.
// It returns nil when all expected containers exist, run, and match images.
func AssertTierState(actual []ContainerState, expected []ContainerResult) *DeployError {
	rows, mismatches := compareTierState(actual, expected)
	if mismatches == 0 {
		return nil
	}
	return &DeployError{
		Phase:   DeployErrorPhasePostcondition,
		Reason:  DeployErrorReasonPostconditionMismatch,
		Message: "container state mismatch",
		Tiers: []TierResult{{
			Name:       "postcondition",
			Status:     TierFailed,
			Containers: rows,
		}},
	}
}

func compareTierState(actual []ContainerState, expected []ContainerResult) ([]ContainerResult, int) {
	actualByName := make(map[string]ContainerState, len(actual))
	for _, container := range actual {
		actualByName[container.ContainerName] = container
	}

	rows := make([]ContainerResult, 0, len(expected))
	mismatches := 0
	for _, want := range expected {
		got := ContainerResult{
			MachineID:     want.MachineID,
			ContainerName: want.ContainerName,
			Expected:      fmt.Sprintf("running image=%s", want.Expected),
		}

		state, ok := actualByName[want.ContainerName]
		if !ok {
			got.Actual = "missing"
			got.Match = false
			mismatches++
			rows = append(rows, got)
			continue
		}

		if !state.Running {
			got.Actual = fmt.Sprintf("stopped image=%s", state.Image)
			got.Match = false
			mismatches++
			rows = append(rows, got)
			continue
		}

		if state.Image != want.Expected {
			got.Actual = fmt.Sprintf("running image=%s", state.Image)
			got.Match = false
			mismatches++
			rows = append(rows, got)
			continue
		}

		got.Actual = fmt.Sprintf("running image=%s", state.Image)
		got.Match = true
		rows = append(rows, got)
	}

	return rows, mismatches
}
