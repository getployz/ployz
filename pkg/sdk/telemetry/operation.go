package telemetry

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"

	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/codes"
	"go.opentelemetry.io/otel/trace"
)

const (
	PlanEventName      = "ployz.plan"
	PlanVersion        = "1"
	PlanVersionKey     = "ployz.plan.version"
	PlanJSONKey        = "ployz.plan.json"
	defaultOperationID = "operation"
)

type PlannedStep struct {
	ID       string `json:"id"`
	ParentID string `json:"parent_id,omitempty"`
	Title    string `json:"title"`
}

type Plan struct {
	Steps []PlannedStep `json:"steps"`
}

type Operation struct {
	ctx    context.Context
	tracer trace.Tracer
	span   trace.Span
}

func EmitPlan(ctx context.Context, tracer trace.Tracer, operation string, plan Plan) (*Operation, error) {
	if tracer == nil {
		return nil, fmt.Errorf("emit telemetry plan: tracer is required")
	}
	if err := validatePlan(plan); err != nil {
		return nil, fmt.Errorf("emit telemetry plan: %w", err)
	}

	operation = strings.TrimSpace(operation)
	if operation == "" {
		operation = defaultOperationID
	}

	planJSON, err := json.Marshal(plan)
	if err != nil {
		return nil, fmt.Errorf("emit telemetry plan: marshal plan: %w", err)
	}

	spanCtx, span := tracer.Start(ctx, operation, trace.WithAttributes(
		attribute.String(PlanVersionKey, PlanVersion),
		attribute.String(PlanJSONKey, string(planJSON)),
	))
	span.AddEvent(PlanEventName, trace.WithAttributes(
		attribute.String(PlanVersionKey, PlanVersion),
		attribute.String(PlanJSONKey, string(planJSON)),
	))

	return &Operation{ctx: spanCtx, tracer: tracer, span: span}, nil
}

func (o *Operation) Context() context.Context {
	if o == nil {
		return context.Background()
	}
	return o.ctx
}

func (o *Operation) RunStep(ctx context.Context, id string, fn func(context.Context) error) error {
	if fn == nil {
		return nil
	}

	stepID := strings.TrimSpace(id)
	if stepID == "" {
		return fmt.Errorf("run telemetry step: step id is required")
	}
	if o == nil || o.tracer == nil {
		return fn(ctx)
	}

	if ctx == nil {
		ctx = o.ctx
	}
	if ctx == nil {
		ctx = context.Background()
	}

	stepCtx, span := o.tracer.Start(ctx, stepID)
	defer span.End()

	err := fn(stepCtx)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, strings.TrimSpace(err.Error()))
		return err
	}
	return nil
}

func (o *Operation) End(err error) {
	if o == nil || o.span == nil {
		return
	}
	if err != nil {
		o.span.RecordError(err)
		o.span.SetStatus(codes.Error, strings.TrimSpace(err.Error()))
	}
	o.span.End()
}

func validatePlan(plan Plan) error {
	indexByID := make(map[string]struct{}, len(plan.Steps))
	for i, step := range plan.Steps {
		stepID := strings.TrimSpace(step.ID)
		if stepID == "" {
			return fmt.Errorf("step %d has empty id", i)
		}
		if _, exists := indexByID[stepID]; exists {
			return fmt.Errorf("duplicate step id %q", stepID)
		}
		indexByID[stepID] = struct{}{}
	}
	for i, step := range plan.Steps {
		parentID := strings.TrimSpace(step.ParentID)
		if parentID == "" {
			continue
		}
		if _, exists := indexByID[parentID]; !exists {
			return fmt.Errorf("step %d parent %q not found in plan", i, parentID)
		}
	}
	return nil
}
