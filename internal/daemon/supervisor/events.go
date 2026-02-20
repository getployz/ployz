package supervisor

import (
	"context"
	"sync"
	"time"

	pb "ployz/internal/daemon/pb"
)

type eventHub struct {
	mu     sync.Mutex
	nextID int
	subs   map[string]map[int]chan *pb.Event
}

func newEventHub() *eventHub {
	return &eventHub{subs: make(map[string]map[int]chan *pb.Event)}
}

func (h *eventHub) publish(network, eventType, message string) {
	h.mu.Lock()
	defer h.mu.Unlock()

	netSubs := h.subs[network]
	if len(netSubs) == 0 {
		return
	}
	ev := &pb.Event{
		Type:    eventType,
		Network: network,
		Message: message,
		At:      time.Now().UTC().Format(time.RFC3339),
	}
	for _, ch := range netSubs {
		select {
		case ch <- ev:
		default:
		}
	}
}

func (h *eventHub) subscribe(ctx context.Context, network string) <-chan *pb.Event {
	ch := make(chan *pb.Event, 128)

	h.mu.Lock()
	if h.subs[network] == nil {
		h.subs[network] = make(map[int]chan *pb.Event)
	}
	id := h.nextID
	h.nextID++
	h.subs[network][id] = ch
	h.mu.Unlock()

	go func() {
		<-ctx.Done()
		h.mu.Lock()
		if m := h.subs[network]; m != nil {
			if sub, ok := m[id]; ok {
				delete(m, id)
				close(sub)
			}
			if len(m) == 0 {
				delete(h.subs, network)
			}
		}
		h.mu.Unlock()
	}()

	return ch
}
