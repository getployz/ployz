package service

import "fmt"

const deployUnavailableMessage = "deploy is being rebuilt - not yet available"

func deployUnavailableError() error {
	return fmt.Errorf("%s", deployUnavailableMessage)
}
