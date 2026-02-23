package deploy

import (
	"crypto/rand"
	"encoding/hex"
	"fmt"
)

const (
	containerNameRandomBytes = 2
	containerNameMaxLen      = 255
)

// ContainerName generates a container name with a random suffix.
// Format: ployz-{namespace}-{service}-{4-char-random}
func ContainerName(namespace, service string) string {
	suffix := randomContainerSuffix()
	namespace, service = truncateNameParts(namespace, service, suffix)
	return fmt.Sprintf("ployz-%s-%s-%s", namespace, service, suffix)
}

func randomContainerSuffix() string {
	b := make([]byte, containerNameRandomBytes)
	if _, err := rand.Read(b); err != nil {
		return fmt.Sprintf("%0*x", containerNameRandomBytes*2, 0)
	}
	return hex.EncodeToString(b)
}

func truncateNameParts(namespace, service, suffix string) (string, string) {
	const fixedLen = len("ployz---")
	maxPartsLen := containerNameMaxLen - fixedLen - len(suffix)
	if maxPartsLen <= 0 {
		return "", ""
	}
	if len(namespace)+len(service) <= maxPartsLen {
		return namespace, service
	}

	over := len(namespace) + len(service) - maxPartsLen
	namespaceLen := len(namespace)
	if over < namespaceLen {
		return namespace[:namespaceLen-over], service
	}

	namespace = ""
	over -= namespaceLen
	if over < len(service) {
		service = service[:len(service)-over]
		return namespace, service
	}

	return namespace, ""
}
