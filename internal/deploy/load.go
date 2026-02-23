package deploy

import (
	"context"
	"fmt"
	"strings"

	"github.com/compose-spec/compose-go/v2/loader"
	compose "github.com/compose-spec/compose-go/v2/types"
)

const composeSpecFilename = "compose.yaml"

// LoadSpec parses a Docker Compose YAML spec into a compose Project.
func LoadSpec(ctx context.Context, data []byte, namespace string) (*compose.Project, error) {
	configDetails := compose.ConfigDetails{
		ConfigFiles: []compose.ConfigFile{
			{Filename: composeSpecFilename, Content: data},
		},
	}

	project, err := loader.LoadWithContext(ctx, configDetails)
	if err != nil {
		return nil, fmt.Errorf("parse compose spec: %w", err)
	}
	if len(project.Services) == 0 {
		return nil, fmt.Errorf("compose spec has no services")
	}
	if trimmed := strings.TrimSpace(namespace); trimmed != "" {
		project.Name = trimmed
	}

	return project, nil
}
