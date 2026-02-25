package ui

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"strings"
	"sync"

	"ployz/pkg/sdk/telemetry"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/codes"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.opentelemetry.io/otel/trace"
)

type TelemetryOutput struct {
	provider *sdktrace.TracerProvider
	closeFn  func()
}

func NewTelemetryOutput() *TelemetryOutput {
	if IsInteractive() {
		checklist := NewChecklist()
		observer := newStepObserver(checklist.OnSnapshot)
		provider := sdktrace.NewTracerProvider(sdktrace.WithSpanProcessor(&stepSpanProcessor{observer: observer}))
		return &TelemetryOutput{provider: provider, closeFn: checklist.Close}
	}

	line := newLineTelemetry()
	observer := newStepObserver(line.OnSnapshot)
	provider := sdktrace.NewTracerProvider(sdktrace.WithSpanProcessor(&stepSpanProcessor{observer: observer}))
	return &TelemetryOutput{provider: provider, closeFn: func() {}}
}

func (o *TelemetryOutput) Tracer(name string) trace.Tracer {
	if o == nil || o.provider == nil {
		return otel.Tracer(name)
	}
	return o.provider.Tracer(name)
}

func (o *TelemetryOutput) Close() {
	if o == nil {
		return
	}
	if o.provider != nil {
		_ = o.provider.Shutdown(context.Background())
	}
	if o.closeFn != nil {
		o.closeFn()
	}
}

type lineTelemetry struct {
	mu       sync.Mutex
	status   map[string]stepStatus
	messages map[string]string
}

func newLineTelemetry() *lineTelemetry {
	return &lineTelemetry{
		status:   make(map[string]stepStatus),
		messages: make(map[string]string),
	}
}

func (l *lineTelemetry) OnSnapshot(snapshot stepSnapshot) {
	l.mu.Lock()
	defer l.mu.Unlock()

	for _, step := range snapshot.Steps {
		if step.Status == stepPending {
			continue
		}

		stepID := strings.TrimSpace(step.ID)
		if stepID == "" {
			stepID = strings.TrimSpace(step.Title)
		}
		if stepID == "" {
			continue
		}

		msg := strings.TrimSpace(step.Message)
		prevStatus, hasStatus := l.status[stepID]
		prevMsg := l.messages[stepID]
		if hasStatus && prevStatus == step.Status && prevMsg == msg {
			continue
		}

		l.status[stepID] = step.Status
		l.messages[stepID] = msg
		fmt.Fprintln(os.Stderr, formatStepLine(step, msg))
	}
}

func formatStepLine(step stepState, msg string) string {
	prefix := "[..]"
	switch step.Status {
	case stepRunning:
		prefix = "[->]"
	case stepDone:
		prefix = "[ok]"
	case stepFailed:
		prefix = "[x]"
	}

	indent := "  "
	if strings.TrimSpace(step.ParentID) != "" {
		indent = "    "
	}

	title := strings.TrimSpace(step.Title)
	if title == "" {
		title = strings.TrimSpace(step.ID)
	}
	if msg != "" {
		return fmt.Sprintf("%s%s %s (%s)", indent, prefix, title, msg)
	}
	return fmt.Sprintf("%s%s %s", indent, prefix, title)
}

type stepObserver struct {
	mu       sync.Mutex
	steps    map[string]stepState
	order    []string
	reporter func(stepSnapshot)
}

func newStepObserver(reporter func(stepSnapshot)) *stepObserver {
	return &stepObserver{
		steps:    make(map[string]stepState),
		order:    make([]string, 0, 8),
		reporter: reporter,
	}
}

func (o *stepObserver) onPlan(plan telemetry.Plan) {
	o.mu.Lock()
	defer o.mu.Unlock()

	for _, planned := range plan.Steps {
		stepID := strings.TrimSpace(planned.ID)
		if stepID == "" {
			continue
		}

		step, exists := o.steps[stepID]
		if !exists {
			o.order = append(o.order, stepID)
			step = stepState{ID: stepID, Status: stepPending}
		}
		step.ParentID = strings.TrimSpace(planned.ParentID)
		step.Title = strings.TrimSpace(planned.Title)
		if step.Title == "" {
			step.Title = stepID
		}
		step.synthetic = false
		o.steps[stepID] = step
	}

	o.emitLocked()
}

func (o *stepObserver) onStepStart(stepID string) {
	o.mu.Lock()
	defer o.mu.Unlock()

	step := o.ensureStepLocked(stepID)
	step.Status = stepRunning
	step.Message = ""
	step.synthetic = false
	o.steps[step.ID] = step
	o.emitLocked()
}

func (o *stepObserver) onStepEnd(stepID string, failed bool, message string) {
	o.mu.Lock()
	defer o.mu.Unlock()

	step := o.ensureStepLocked(stepID)
	step.synthetic = false
	if failed {
		step.Status = stepFailed
		step.Message = strings.TrimSpace(message)
	} else {
		step.Status = stepDone
		step.Message = ""
	}
	o.steps[step.ID] = step
	o.emitLocked()
}

func (o *stepObserver) ensureStepLocked(stepID string) stepState {
	stepID = strings.TrimSpace(stepID)
	if stepID == "" {
		stepID = "unnamed"
	}

	if step, exists := o.steps[stepID]; exists {
		return step
	}

	parentID := ""
	if idx := strings.LastIndex(stepID, "/"); idx > 0 {
		parentID = strings.TrimSpace(stepID[:idx])
		o.ensureParentLocked(parentID)
	}

	o.order = append(o.order, stepID)
	return stepState{ID: stepID, ParentID: parentID, Title: stepID, Status: stepPending}
}

func (o *stepObserver) ensureParentLocked(parentID string) {
	parentID = strings.TrimSpace(parentID)
	if parentID == "" {
		return
	}
	if _, exists := o.steps[parentID]; exists {
		return
	}

	ancestorID := ""
	if idx := strings.LastIndex(parentID, "/"); idx > 0 {
		ancestorID = strings.TrimSpace(parentID[:idx])
		o.ensureParentLocked(ancestorID)
	}

	o.order = append(o.order, parentID)
	o.steps[parentID] = stepState{
		ID:        parentID,
		ParentID:  ancestorID,
		Title:     parentID,
		Status:    stepPending,
		synthetic: true,
	}
}

func (o *stepObserver) emitLocked() {
	if o.reporter == nil {
		return
	}

	childrenByParent := make(map[string][]stepState, len(o.steps))
	for _, step := range o.steps {
		parentID := strings.TrimSpace(step.ParentID)
		if parentID == "" {
			continue
		}
		childrenByParent[parentID] = append(childrenByParent[parentID], step)
	}

	steps := make([]stepState, 0, len(o.order))
	for _, stepID := range o.order {
		step, exists := o.steps[stepID]
		if !exists {
			continue
		}

		children := childrenByParent[step.ID]
		if len(children) > 0 {
			if step.synthetic {
				step.Status = deriveSyntheticParentStatus(children)
			}
			summary := summarizeFanout(children)
			if strings.TrimSpace(summary) != "" {
				if strings.TrimSpace(step.Message) == "" {
					step.Message = summary
				} else if step.Status == stepFailed && !strings.Contains(step.Message, summary) {
					step.Message = summary + "; " + step.Message
				}
			}
		}

		steps = append(steps, step)
	}
	o.reporter(stepSnapshot{Steps: steps})
}

func summarizeFanout(children []stepState) string {
	total := len(children)
	if total == 0 {
		return ""
	}

	doneCount := 0
	failedCount := 0
	for _, child := range children {
		switch child.Status {
		case stepDone:
			doneCount++
		case stepFailed:
			failedCount++
		}
	}

	if failedCount > 0 {
		return fmt.Sprintf("%d/%d done, %d failed", doneCount, total, failedCount)
	}
	if doneCount == 0 {
		return fmt.Sprintf("%d discovered", total)
	}
	return fmt.Sprintf("%d/%d done", doneCount, total)
}

func deriveSyntheticParentStatus(children []stepState) stepStatus {
	if len(children) == 0 {
		return stepPending
	}

	hasRunning := false
	hasFailed := false
	doneCount := 0
	for _, child := range children {
		switch child.Status {
		case stepFailed:
			hasFailed = true
		case stepRunning:
			hasRunning = true
		case stepDone:
			doneCount++
		}
	}

	if hasFailed {
		return stepFailed
	}
	if doneCount == len(children) {
		return stepDone
	}
	if hasRunning || doneCount > 0 {
		return stepRunning
	}
	return stepPending
}

type stepSpanProcessor struct {
	observer *stepObserver
}

func (p *stepSpanProcessor) OnStart(_ context.Context, span sdktrace.ReadWriteSpan) {
	if p == nil || p.observer == nil {
		return
	}

	if span.Parent().IsValid() {
		p.observer.onStepStart(span.Name())
		return
	}

	planJSON := attributeValue(span.Attributes(), telemetry.PlanJSONKey)
	if strings.TrimSpace(planJSON) == "" {
		return
	}

	var plan telemetry.Plan
	if err := json.Unmarshal([]byte(planJSON), &plan); err != nil {
		return
	}
	p.observer.onPlan(plan)
}

func (p *stepSpanProcessor) OnEnd(span sdktrace.ReadOnlySpan) {
	if p == nil || p.observer == nil {
		return
	}
	if !span.Parent().IsValid() {
		return
	}

	status := span.Status()
	failed := status.Code == codes.Error
	message := strings.TrimSpace(status.Description)
	p.observer.onStepEnd(span.Name(), failed, message)
}

func (p *stepSpanProcessor) Shutdown(context.Context) error {
	return nil
}

func (p *stepSpanProcessor) ForceFlush(context.Context) error {
	return nil
}

func attributeValue(attrs []attribute.KeyValue, key string) string {
	for _, attr := range attrs {
		if string(attr.Key) == key {
			return attr.Value.AsString()
		}
	}
	return ""
}
