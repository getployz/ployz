package deploy

import (
	"regexp"
	"testing"
)

func TestContainerName_Format(t *testing.T) {
	name := ContainerName("frontend", "web")
	re := regexp.MustCompile(`^ployz-frontend-web-[0-9a-f]{4}$`)
	if !re.MatchString(name) {
		t.Fatalf("ContainerName() = %q, expected pattern %q", name, re.String())
	}
}

func TestContainerName_UniqueAcrossCalls(t *testing.T) {
	first := ContainerName("frontend", "web")
	unique := false
	for range 8 {
		if next := ContainerName("frontend", "web"); next != first {
			unique = true
			break
		}
	}
	if !unique {
		t.Fatalf("expected random suffix to vary across calls, first=%q", first)
	}
}

func TestContainerName_LengthBounded(t *testing.T) {
	longNamespace := "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
	longService := "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"

	name := ContainerName(longNamespace, longService)
	if len(name) > containerNameMaxLen {
		t.Fatalf("ContainerName() length = %d, max %d", len(name), containerNameMaxLen)
	}
}
