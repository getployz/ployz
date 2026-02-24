package watch

import (
	"context"
	"log/slog"
	"sync"
	"time"

	"ployz/internal/check"
	"ployz/internal/network"
)

const (
	watchSubscriberBufferCap  = 128
	watchReplayBufferCapacity = 256
	watchResubscribeDelay     = 1 * time.Second
)

type Source interface {
	SubscribeMachines(ctx context.Context) ([]network.MachineRow, <-chan network.MachineChange, error)
	SubscribeHeartbeats(ctx context.Context) ([]network.HeartbeatRow, <-chan network.HeartbeatChange, error)
}

type Broker struct {
	source Source

	machines machineTopic
	hearts   heartbeatTopic
}

func NewBroker(source Source) *Broker {
	check.Assert(source != nil, "watch.NewBroker: source must not be nil")
	return &Broker{source: source}
}

func (b *Broker) SubscribeMachines(ctx context.Context) ([]network.MachineRow, <-chan network.MachineChange, error) {
	b.machines.mu.Lock()
	if b.machines.subs == nil {
		b.machines.subs = make(map[uint64]chan network.MachineChange)
	}
	id := b.machines.nextID
	b.machines.nextID++
	ch := make(chan network.MachineChange, watchSubscriberBufferCap)
	b.machines.subs[id] = ch
	needStart := b.machines.cancel == nil
	replay := append([]network.MachineChange(nil), b.machines.replay...)
	snapshot := append([]network.MachineRow(nil), b.machines.snapshot...)
	b.machines.mu.Unlock()

	if needStart {
		if err := b.startMachines(); err != nil {
			b.unsubscribeMachines(id)
			return nil, nil, err
		}
		b.machines.mu.Lock()
		replay = append([]network.MachineChange(nil), b.machines.replay...)
		snapshot = append([]network.MachineRow(nil), b.machines.snapshot...)
		b.machines.mu.Unlock()
	}

	go b.watchMachineSubscriber(ctx, id, ch, replay)
	return snapshot, ch, nil
}

func (b *Broker) SubscribeHeartbeats(ctx context.Context) ([]network.HeartbeatRow, <-chan network.HeartbeatChange, error) {
	b.hearts.mu.Lock()
	if b.hearts.subs == nil {
		b.hearts.subs = make(map[uint64]chan network.HeartbeatChange)
	}
	id := b.hearts.nextID
	b.hearts.nextID++
	ch := make(chan network.HeartbeatChange, watchSubscriberBufferCap)
	b.hearts.subs[id] = ch
	needStart := b.hearts.cancel == nil
	replay := append([]network.HeartbeatChange(nil), b.hearts.replay...)
	snapshot := append([]network.HeartbeatRow(nil), b.hearts.snapshot...)
	b.hearts.mu.Unlock()

	if needStart {
		if err := b.startHeartbeats(); err != nil {
			b.unsubscribeHeartbeats(id)
			return nil, nil, err
		}
		b.hearts.mu.Lock()
		replay = append([]network.HeartbeatChange(nil), b.hearts.replay...)
		snapshot = append([]network.HeartbeatRow(nil), b.hearts.snapshot...)
		b.hearts.mu.Unlock()
	}

	go b.watchHeartbeatSubscriber(ctx, id, ch, replay)
	return snapshot, ch, nil
}

func (b *Broker) startMachines() error {
	b.machines.mu.Lock()
	if b.machines.cancel != nil {
		b.machines.mu.Unlock()
		return nil
	}
	topicCtx, cancel := context.WithCancel(context.Background())
	b.machines.cancel = cancel
	b.machines.done = make(chan struct{})
	b.machines.mu.Unlock()

	snapshot, changes, err := b.source.SubscribeMachines(topicCtx)
	if err != nil {
		cancel()
		b.machines.mu.Lock()
		b.machines.cancel = nil
		b.machines.done = nil
		b.machines.snapshot = nil
		b.machines.replay = nil
		b.machines.mu.Unlock()
		return err
	}

	b.machines.mu.Lock()
	b.machines.snapshot = append([]network.MachineRow(nil), snapshot...)
	b.machines.mu.Unlock()

	go b.runMachines(topicCtx, changes)
	slog.Debug("watch topic started", "topic", TopicMachines)
	return nil
}

func (b *Broker) startHeartbeats() error {
	b.hearts.mu.Lock()
	if b.hearts.cancel != nil {
		b.hearts.mu.Unlock()
		return nil
	}
	topicCtx, cancel := context.WithCancel(context.Background())
	b.hearts.cancel = cancel
	b.hearts.done = make(chan struct{})
	b.hearts.mu.Unlock()

	snapshot, changes, err := b.source.SubscribeHeartbeats(topicCtx)
	if err != nil {
		cancel()
		b.hearts.mu.Lock()
		b.hearts.cancel = nil
		b.hearts.done = nil
		b.hearts.snapshot = nil
		b.hearts.replay = nil
		b.hearts.mu.Unlock()
		return err
	}

	b.hearts.mu.Lock()
	b.hearts.snapshot = append([]network.HeartbeatRow(nil), snapshot...)
	b.hearts.mu.Unlock()

	go b.runHeartbeats(topicCtx, changes)
	slog.Debug("watch topic started", "topic", TopicHeartbeats)
	return nil
}

func (b *Broker) runMachines(ctx context.Context, changes <-chan network.MachineChange) {
	defer b.finishMachines()
	for {
		select {
		case <-ctx.Done():
			return
		case change, ok := <-changes:
			if !ok {
				nextSnapshot, nextChanges, err := b.source.SubscribeMachines(ctx)
				if err != nil {
					if ctx.Err() != nil {
						return
					}
					if !sleepContext(ctx, watchResubscribeDelay) {
						return
					}
					continue
				}
				b.machines.mu.Lock()
				b.machines.snapshot = append([]network.MachineRow(nil), nextSnapshot...)
				b.machines.replay = nil
				b.machines.mu.Unlock()
				changes = nextChanges
				continue
			}
			b.publishMachineChange(change)
		}
	}
}

func (b *Broker) runHeartbeats(ctx context.Context, changes <-chan network.HeartbeatChange) {
	defer b.finishHeartbeats()
	for {
		select {
		case <-ctx.Done():
			return
		case change, ok := <-changes:
			if !ok {
				nextSnapshot, nextChanges, err := b.source.SubscribeHeartbeats(ctx)
				if err != nil {
					if ctx.Err() != nil {
						return
					}
					if !sleepContext(ctx, watchResubscribeDelay) {
						return
					}
					continue
				}
				b.hearts.mu.Lock()
				b.hearts.snapshot = append([]network.HeartbeatRow(nil), nextSnapshot...)
				b.hearts.replay = nil
				b.hearts.mu.Unlock()
				changes = nextChanges
				continue
			}
			b.publishHeartbeatChange(change)
		}
	}
}

func (b *Broker) publishMachineChange(change network.MachineChange) {
	b.machines.mu.Lock()
	b.machines.replay = appendMachineReplay(b.machines.replay, change)
	for _, sub := range b.machines.subs {
		select {
		case sub <- change:
		default:
		}
	}
	b.machines.mu.Unlock()
}

func (b *Broker) publishHeartbeatChange(change network.HeartbeatChange) {
	b.hearts.mu.Lock()
	b.hearts.replay = appendHeartbeatReplay(b.hearts.replay, change)
	for _, sub := range b.hearts.subs {
		select {
		case sub <- change:
		default:
		}
	}
	b.hearts.mu.Unlock()
}

func (b *Broker) watchMachineSubscriber(ctx context.Context, id uint64, ch chan network.MachineChange, replay []network.MachineChange) {
	for _, change := range replay {
		select {
		case ch <- change:
		default:
		}
	}
	<-ctx.Done()
	b.unsubscribeMachines(id)
}

func (b *Broker) watchHeartbeatSubscriber(ctx context.Context, id uint64, ch chan network.HeartbeatChange, replay []network.HeartbeatChange) {
	for _, change := range replay {
		select {
		case ch <- change:
		default:
		}
	}
	<-ctx.Done()
	b.unsubscribeHeartbeats(id)
}

func (b *Broker) unsubscribeMachines(id uint64) {
	shouldStop := false
	b.machines.mu.Lock()
	ch, ok := b.machines.subs[id]
	if ok {
		delete(b.machines.subs, id)
		close(ch)
	}
	if len(b.machines.subs) == 0 {
		shouldStop = true
	}
	b.machines.mu.Unlock()

	if shouldStop {
		b.stopMachinesIfIdle()
	}
}

func (b *Broker) unsubscribeHeartbeats(id uint64) {
	shouldStop := false
	b.hearts.mu.Lock()
	ch, ok := b.hearts.subs[id]
	if ok {
		delete(b.hearts.subs, id)
		close(ch)
	}
	if len(b.hearts.subs) == 0 {
		shouldStop = true
	}
	b.hearts.mu.Unlock()

	if shouldStop {
		b.stopHeartbeatsIfIdle()
	}
}

func (b *Broker) stopMachinesIfIdle() {
	b.machines.mu.Lock()
	if len(b.machines.subs) != 0 {
		b.machines.mu.Unlock()
		return
	}
	cancel := b.machines.cancel
	done := b.machines.done
	b.machines.cancel = nil
	b.machines.done = nil
	b.machines.snapshot = nil
	b.machines.replay = nil
	if cancel == nil {
		b.machines.mu.Unlock()
		return
	}
	b.machines.mu.Unlock()

	cancel()
	if done != nil {
		<-done
	}
	slog.Debug("watch topic stopped", "topic", TopicMachines)
}

func (b *Broker) stopHeartbeatsIfIdle() {
	b.hearts.mu.Lock()
	if len(b.hearts.subs) != 0 {
		b.hearts.mu.Unlock()
		return
	}
	cancel := b.hearts.cancel
	done := b.hearts.done
	b.hearts.cancel = nil
	b.hearts.done = nil
	b.hearts.snapshot = nil
	b.hearts.replay = nil
	if cancel == nil {
		b.hearts.mu.Unlock()
		return
	}
	b.hearts.mu.Unlock()

	cancel()
	if done != nil {
		<-done
	}
	slog.Debug("watch topic stopped", "topic", TopicHeartbeats)
}

func (b *Broker) finishMachines() {
	b.machines.mu.Lock()
	if b.machines.done != nil {
		close(b.machines.done)
	}
	b.machines.mu.Unlock()
}

func (b *Broker) finishHeartbeats() {
	b.hearts.mu.Lock()
	if b.hearts.done != nil {
		close(b.hearts.done)
	}
	b.hearts.mu.Unlock()
}

func appendMachineReplay(replay []network.MachineChange, change network.MachineChange) []network.MachineChange {
	if len(replay) < watchReplayBufferCapacity {
		return append(replay, change)
	}
	copy(replay, replay[1:])
	replay[len(replay)-1] = change
	return replay
}

func appendHeartbeatReplay(replay []network.HeartbeatChange, change network.HeartbeatChange) []network.HeartbeatChange {
	if len(replay) < watchReplayBufferCapacity {
		return append(replay, change)
	}
	copy(replay, replay[1:])
	replay[len(replay)-1] = change
	return replay
}

func sleepContext(ctx context.Context, d time.Duration) bool {
	if d <= 0 {
		return ctx.Err() == nil
	}
	timer := time.NewTimer(d)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return false
	case <-timer.C:
		return true
	}
}

type machineTopic struct {
	mu       sync.Mutex
	subs     map[uint64]chan network.MachineChange
	nextID   uint64
	snapshot []network.MachineRow
	replay   []network.MachineChange
	cancel   context.CancelFunc
	done     chan struct{}
}

type heartbeatTopic struct {
	mu       sync.Mutex
	subs     map[uint64]chan network.HeartbeatChange
	nextID   uint64
	snapshot []network.HeartbeatRow
	replay   []network.HeartbeatChange
	cancel   context.CancelFunc
	done     chan struct{}
}
