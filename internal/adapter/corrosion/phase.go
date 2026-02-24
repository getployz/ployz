package corrosion

import "ployz/internal/check"

type SubscriptionPhase uint8

const (
	SubscriptionOpening SubscriptionPhase = iota + 1
	SubscriptionStreaming
	SubscriptionResubscribing
	SubscriptionClosedExhausted
	SubscriptionClosedContext
)

func (p SubscriptionPhase) String() string {
	switch p {
	case SubscriptionOpening:
		return "opening"
	case SubscriptionStreaming:
		return "streaming"
	case SubscriptionResubscribing:
		return "resubscribing"
	case SubscriptionClosedExhausted:
		return "closed_exhausted"
	case SubscriptionClosedContext:
		return "closed_context"
	default:
		return "unknown"
	}
}

func (p SubscriptionPhase) Transition(to SubscriptionPhase) SubscriptionPhase {
	ok := false
	switch p {
	case SubscriptionOpening:
		ok = to == SubscriptionStreaming || to == SubscriptionClosedExhausted || to == SubscriptionClosedContext
	case SubscriptionStreaming:
		ok = to == SubscriptionResubscribing || to == SubscriptionClosedContext || to == SubscriptionClosedExhausted
	case SubscriptionResubscribing:
		ok = to == SubscriptionStreaming || to == SubscriptionClosedExhausted || to == SubscriptionClosedContext
	case SubscriptionClosedExhausted, SubscriptionClosedContext:
		ok = false
	}
	check.Assertf(ok, "subscription transition: %s -> %s", p, to)
	if !ok {
		return p
	}
	return to
}
