package telemetry

import (
	"context"
	"errors"
	"testing"

	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/codes"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.opentelemetry.io/otel/sdk/trace/tracetest"
	"go.opentelemetry.io/otel/trace"
)

func TestEmitPlanAndRunStepSuccess(t *testing.T) {
	t.Parallel()

	tracer, recorder := newTestTracer()
	op, err := EmitPlan(context.Background(), tracer, "machine.add", Plan{Steps: []PlannedStep{
		{ID: "install", Title: "installing"},
		{ID: "connect", ParentID: "install", Title: "connecting"},
	}})
	if err != nil {
		t.Fatalf("EmitPlan() error = %v", err)
	}

	if err := op.RunStep(op.Context(), "install", func(context.Context) error { return nil }); err != nil {
		t.Fatalf("RunStep() error = %v", err)
	}
	op.End(nil)

	spans := recorder.Ended()
	if len(spans) != 2 {
		t.Fatalf("ended span count = %d, want 2", len(spans))
	}

	root := findSpanByName(spans, "machine.add")
	if root == nil {
		t.Fatal("missing root span")
	}
	if len(root.Events()) == 0 {
		t.Fatal("expected root plan event")
	}
	planEvent := root.Events()[0]
	if planEvent.Name != PlanEventName {
		t.Fatalf("plan event name = %q, want %q", planEvent.Name, PlanEventName)
	}
	if getAttr(planEvent.Attributes, PlanVersionKey) != PlanVersion {
		t.Fatalf("plan event version = %q, want %q", getAttr(planEvent.Attributes, PlanVersionKey), PlanVersion)
	}

	child := findSpanByName(spans, "install")
	if child == nil {
		t.Fatal("missing child step span")
	}
	if child.Parent().SpanID() != root.SpanContext().SpanID() {
		t.Fatalf("step parent span id = %s, want %s", child.Parent().SpanID(), root.SpanContext().SpanID())
	}
}

func TestRunStepFailureSetsErrorStatus(t *testing.T) {
	t.Parallel()

	tracer, recorder := newTestTracer()
	op, err := EmitPlan(context.Background(), tracer, "configure", Plan{Steps: []PlannedStep{{ID: "configure_helper", Title: "configure helper"}}})
	if err != nil {
		t.Fatalf("EmitPlan() error = %v", err)
	}

	boom := errors.New("boom")
	err = op.RunStep(op.Context(), "configure_helper", func(context.Context) error {
		return boom
	})
	if !errors.Is(err, boom) {
		t.Fatalf("RunStep() error = %v, want boom", err)
	}
	op.End(err)

	spans := recorder.Ended()
	child := findSpanByName(spans, "configure_helper")
	if child == nil {
		t.Fatal("missing failed step span")
	}
	if child.Status().Code != codes.Error {
		t.Fatalf("step status code = %v, want %v", child.Status().Code, codes.Error)
	}
	if child.Status().Description != "boom" {
		t.Fatalf("step status description = %q, want boom", child.Status().Description)
	}
}

func TestEmitPlanValidationFailure(t *testing.T) {
	t.Parallel()

	tracer, _ := newTestTracer()
	_, err := EmitPlan(context.Background(), tracer, "machine.add", Plan{Steps: []PlannedStep{
		{ID: "install", Title: "installing"},
		{ID: "install", Title: "duplicated"},
	}})
	if err == nil {
		t.Fatal("EmitPlan() error = nil, want duplicate id error")
	}
}

func newTestTracer() (trace.Tracer, *tracetest.SpanRecorder) {
	recorder := tracetest.NewSpanRecorder()
	provider := sdktrace.NewTracerProvider(sdktrace.WithSpanProcessor(recorder))
	return provider.Tracer("telemetry-test"), recorder
}

func findSpanByName(spans []sdktrace.ReadOnlySpan, name string) sdktrace.ReadOnlySpan {
	for _, span := range spans {
		if span.Name() == name {
			return span
		}
	}
	return nil
}

func getAttr(attrs []attribute.KeyValue, key string) string {
	for _, attr := range attrs {
		if string(attr.Key) == key {
			return attr.Value.AsString()
		}
	}
	return ""
}
