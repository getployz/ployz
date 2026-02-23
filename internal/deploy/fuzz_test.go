package deploy_test

import (
	"testing"

	"ployz/internal/deploy"
)

func FuzzPostcondition_AlwaysDetectsMismatch(f *testing.F) {
	f.Add("api-1", "api:1", true, "api:1")
	f.Add("api-1", "api:2", true, "api:1")
	f.Add("api-1", "api:1", false, "api:1")

	f.Fuzz(func(t *testing.T, containerName, actualImage string, running bool, expectedImage string) {
		if containerName == "" {
			t.Skip()
		}

		actual := []deploy.ContainerState{{
			ContainerName: containerName,
			Image:         actualImage,
			Running:       running,
		}}
		expected := []deploy.ContainerResult{{
			MachineID:     "m1",
			ContainerName: containerName,
			Expected:      expectedImage,
		}}

		err := deploy.AssertTierState(actual, expected)
		wantMatch := running && actualImage == expectedImage
		if wantMatch && err != nil {
			t.Fatalf("AssertTierState() = %v, want nil (running and image match)", err)
		}
		if !wantMatch && err == nil {
			t.Fatalf("AssertTierState() = nil, want mismatch error (running=%v actual=%q expected=%q)", running, actualImage, expectedImage)
		}
	})
}
