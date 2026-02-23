package fault

import (
	"errors"
	"testing"
)

const testPoint = "registry.upsert_machine"

func TestInjectorFailOnce(t *testing.T) {
	i := NewInjector()
	injected := errors.New("injected once")
	i.FailOnce(testPoint, injected)

	err := i.Eval(testPoint)
	if !errors.Is(err, injected) {
		t.Fatalf("first Eval error = %v, want %v", err, injected)
	}

	err = i.Eval(testPoint)
	if err != nil {
		t.Fatalf("second Eval error = %v, want nil", err)
	}
}

func TestInjectorFailAlways(t *testing.T) {
	i := NewInjector()
	injected := errors.New("injected always")
	i.FailAlways(testPoint, injected)

	err := i.Eval(testPoint)
	if !errors.Is(err, injected) {
		t.Fatalf("first Eval error = %v, want %v", err, injected)
	}

	err = i.Eval(testPoint)
	if !errors.Is(err, injected) {
		t.Fatalf("second Eval error = %v, want %v", err, injected)
	}
}

func TestInjectorHook(t *testing.T) {
	i := NewInjector()
	injected := errors.New("bad arg")
	i.SetHook(testPoint, func(args ...any) error {
		if len(args) == 0 {
			return nil
		}
		name, _ := args[0].(string)
		if name == "bad" {
			return injected
		}
		return nil
	})

	err := i.Eval(testPoint, "bad")
	if !errors.Is(err, injected) {
		t.Fatalf("Eval bad error = %v, want %v", err, injected)
	}

	err = i.Eval(testPoint, "good")
	if err != nil {
		t.Fatalf("Eval good error = %v, want nil", err)
	}
}

func TestInjectorClearAndReset(t *testing.T) {
	i := NewInjector()
	one := errors.New("one")
	two := errors.New("two")
	i.FailAlways("a", one)
	i.FailAlways("b", two)

	i.Clear("a")
	err := i.Eval("a")
	if err != nil {
		t.Fatalf("Eval a after Clear = %v, want nil", err)
	}

	err = i.Eval("b")
	if !errors.Is(err, two) {
		t.Fatalf("Eval b before Reset = %v, want %v", err, two)
	}

	i.Reset()
	err = i.Eval("b")
	if err != nil {
		t.Fatalf("Eval b after Reset = %v, want nil", err)
	}
}
