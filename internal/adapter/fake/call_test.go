package fake

import "testing"

func TestCallRecorder_Record(t *testing.T) {
	var r CallRecorder

	r.record("Foo", "a", 1)
	r.record("Bar", "b")
	r.record("Foo", "c")

	all := r.Calls("")
	if len(all) != 3 {
		t.Fatalf("expected 3 calls, got %d", len(all))
	}

	foos := r.Calls("Foo")
	if len(foos) != 2 {
		t.Fatalf("expected 2 Foo calls, got %d", len(foos))
	}
	if foos[0].Args[0] != "a" {
		t.Errorf("expected first Foo arg 'a', got %v", foos[0].Args[0])
	}

	bars := r.Calls("Bar")
	if len(bars) != 1 {
		t.Fatalf("expected 1 Bar call, got %d", len(bars))
	}

	none := r.Calls("Baz")
	if len(none) != 0 {
		t.Errorf("expected 0 Baz calls, got %d", len(none))
	}
}

func TestCallRecorder_Reset(t *testing.T) {
	var r CallRecorder

	r.record("Foo")
	r.record("Bar")
	r.Reset()

	if len(r.Calls("")) != 0 {
		t.Errorf("expected 0 calls after reset, got %d", len(r.Calls("")))
	}
}
