package ui

import (
	"os"
	"strings"
	"sync"

	"github.com/charmbracelet/lipgloss"
	"github.com/muesli/termenv"
)

const (
	envNoInteraction = "NO_INTERACTION"
	envCI            = "CI"
	envTerm          = "TERM"
)

type interactionConfig struct {
	initialized   bool
	noInteraction bool
	interactive   bool
}

var interactionState struct {
	mu  sync.RWMutex
	cfg interactionConfig
}

func ConfigureInteraction(noInteraction bool) {
	interactive := detectInteractiveMode(noInteraction)

	interactionState.mu.Lock()
	interactionState.cfg = interactionConfig{
		initialized:   true,
		noInteraction: !interactive,
		interactive:   interactive,
	}
	interactionState.mu.Unlock()

	if interactive {
		lipgloss.SetColorProfile(termenv.ColorProfile())
		return
	}
	lipgloss.SetColorProfile(termenv.Ascii)
}

func IsInteractive() bool {
	interactionState.mu.RLock()
	if interactionState.cfg.initialized {
		interactive := interactionState.cfg.interactive
		interactionState.mu.RUnlock()
		return interactive
	}
	interactionState.mu.RUnlock()

	ConfigureInteraction(false)

	interactionState.mu.RLock()
	interactive := interactionState.cfg.interactive
	interactionState.mu.RUnlock()
	return interactive
}

func IsNoInteraction() bool {
	return !IsInteractive()
}

func detectInteractiveMode(noInteraction bool) bool {
	if noInteraction {
		return false
	}
	if envTruthy(envNoInteraction) || envTruthy(envCI) {
		return false
	}
	if strings.EqualFold(strings.TrimSpace(os.Getenv(envTerm)), "dumb") {
		return false
	}
	return stderrIsTerminal()
}

func stderrIsTerminal() bool {
	info, err := os.Stderr.Stat()
	if err != nil {
		return false
	}
	return (info.Mode() & os.ModeCharDevice) != 0
}

func envTruthy(key string) bool {
	v := strings.TrimSpace(strings.ToLower(os.Getenv(key)))
	switch v {
	case "1", "true", "yes", "on":
		return true
	default:
		return false
	}
}
