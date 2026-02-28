package corrosion

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"strconv"

	"github.com/cenkalti/backoff/v4"
)

// ChangeType describes the kind of change in a subscription event.
type ChangeType string

const (
	ChangeInsert ChangeType = "insert"
	ChangeUpdate ChangeType = "update"
	ChangeDelete ChangeType = "delete"
)

// ChangeEvent is a single change in a subscription stream: [type, rowid, values, changeid].
type ChangeEvent struct {
	Type     ChangeType
	RowID    uint64
	Values   []json.RawMessage
	ChangeID uint64
}

func (ce *ChangeEvent) UnmarshalJSON(data []byte) error {
	var raw [4]json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil {
		return fmt.Errorf("invalid change event: %w", err)
	}
	if err := json.Unmarshal(raw[0], &ce.Type); err != nil {
		return fmt.Errorf("invalid change type: %w", err)
	}
	if err := json.Unmarshal(raw[1], &ce.RowID); err != nil {
		return fmt.Errorf("invalid change rowid: %w", err)
	}
	if err := json.Unmarshal(raw[2], &ce.Values); err != nil {
		return fmt.Errorf("invalid change values: %w", err)
	}
	if err := json.Unmarshal(raw[3], &ce.ChangeID); err != nil {
		return fmt.Errorf("invalid change id: %w", err)
	}
	return nil
}

func (ce *ChangeEvent) MarshalJSON() ([]byte, error) {
	return json.Marshal([]any{ce.Type, ce.RowID, ce.Values, ce.ChangeID})
}

// Scan unmarshals the change event's column values into dest.
func (ce *ChangeEvent) Scan(dest ...any) error {
	if len(dest) != len(ce.Values) {
		return fmt.Errorf("scan change: expected %d values, got %d", len(ce.Values), len(dest))
	}
	for i, v := range ce.Values {
		if err := json.Unmarshal(v, dest[i]); err != nil {
			return fmt.Errorf("scan change column %d: %w", i, err)
		}
	}
	return nil
}

// Subscription receives streaming changes from Corrosion for a SQL query.
type Subscription struct {
	ctx    context.Context
	cancel context.CancelFunc

	id           string
	rows         *Rows
	body         io.ReadCloser
	decoder      *json.Decoder
	resubscribe  func(ctx context.Context, fromChange uint64) (*Subscription, error)
	changes      chan *ChangeEvent
	lastChangeID uint64
	err          error
}

func newSubscription(
	ctx context.Context,
	id string,
	rows *Rows,
	body io.ReadCloser,
	decoder *json.Decoder,
	resubscribe func(ctx context.Context, fromChange uint64) (*Subscription, error),
) *Subscription {
	ctx, cancel := context.WithCancel(ctx)
	if decoder == nil {
		decoder = json.NewDecoder(body)
	}
	return &Subscription{
		ctx:         ctx,
		cancel:      cancel,
		id:          id,
		rows:        rows,
		body:        body,
		decoder:     decoder,
		resubscribe: resubscribe,
	}
}

// ID returns the subscription ID.
func (s *Subscription) ID() string {
	return s.id
}

// Rows returns the initial query rows, or nil if skipRows was true.
func (s *Subscription) Rows() *Rows {
	return s.rows
}

// Changes returns a channel of change events. All initial rows must be
// consumed before calling this. The channel closes on context cancellation
// or error (check Err).
func (s *Subscription) Changes() (<-chan *ChangeEvent, error) {
	if s.changes != nil {
		return s.changes, nil
	}

	if s.rows != nil {
		if s.rows.eoq == nil {
			return nil, errors.New("changes unavailable: consume all rows first")
		}
		s.lastChangeID = *s.rows.eoq.ChangeID
	}
	s.changes = make(chan *ChangeEvent)

	go func() {
		<-s.ctx.Done()
		s.body.Close()
	}()
	go s.readChanges()

	return s.changes, nil
}

func (s *Subscription) readChanges() {
	defer s.cancel()
	defer close(s.changes)

	for {
		select {
		case <-s.ctx.Done():
			return
		default:
		}

		var e QueryEvent
		var err error
		if err = s.decoder.Decode(&e); err != nil {
			if s.ctx.Err() != nil {
				return // context cancelled, not an error
			}
			err = fmt.Errorf("decode change event: %w", err)
		} else if e.Error != nil {
			err = fmt.Errorf("subscription error: %s", *e.Error)
		} else if e.Change == nil {
			err = fmt.Errorf("expected change event, got: %+v", e)
		} else if s.lastChangeID != 0 && e.Change.ChangeID != s.lastChangeID+1 {
			err = fmt.Errorf("missed change: expected %d, got %d",
				s.lastChangeID+1, e.Change.ChangeID)
		}

		if err == nil {
			s.lastChangeID = e.Change.ChangeID
			select {
			case s.changes <- e.Change:
			case <-s.ctx.Done():
				return
			}
			continue
		}

		if s.resubscribe == nil {
			s.err = err
			return
		}

		slog.Info("Resubscribing to Corrosion query.",
			"err", err, "id", s.id, "from_change", s.lastChangeID)
		sub, sErr := s.resubscribe(s.ctx, s.lastChangeID)
		if sErr != nil {
			s.err = fmt.Errorf("resubscribe: %w", sErr)
			return
		}
		s.rows = nil
		s.body = sub.body
		s.decoder = sub.decoder
		sub.cancel() // don't close the body, just detach the child context
	}
}

// Err returns the error that caused the changes channel to close, if any.
func (s *Subscription) Err() error {
	return s.err
}

// Close cancels the subscription and releases resources.
func (s *Subscription) Close() error {
	s.cancel()
	return s.body.Close()
}

// SubscribeContext creates a subscription for a SQL query. If skipRows is false,
// Rows() must be fully consumed before calling Changes().
func (c *Client) SubscribeContext(
	ctx context.Context, query string, args []any, skipRows bool,
) (*Subscription, error) {
	body, err := json.Marshal(Statement{Query: query, Params: args})
	if err != nil {
		return nil, fmt.Errorf("marshal subscription query: %w", err)
	}

	subURL := c.baseURL.JoinPath("/v1/subscriptions")
	if skipRows {
		q := subURL.Query()
		q.Set("skip_rows", "true")
		subURL.RawQuery = q.Encode()
	}

	req, err := http.NewRequestWithContext(ctx, "POST", subURL.String(), bytes.NewReader(body))
	if err != nil {
		return nil, fmt.Errorf("create subscribe request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Accept", "application/json")

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("subscribe: %w", err)
	}

	if resp.StatusCode != http.StatusOK {
		respBody, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
		resp.Body.Close()
		if err != nil {
			return nil, fmt.Errorf("read subscribe response: %w", err)
		}
		return nil, fmt.Errorf("subscribe: unexpected status %d: %s", resp.StatusCode, respBody)
	}

	id := resp.Header.Get("corro-query-id")
	if id == "" {
		resp.Body.Close()
		return nil, errors.New("subscribe: missing corro-query-id header")
	}

	if skipRows {
		return newSubscription(ctx, id, nil, resp.Body, nil, c.resubscribeWithBackoffFn(id)), nil
	}

	rows, err := newRows(ctx, resp.Body, false)
	if err != nil {
		resp.Body.Close()
		return nil, fmt.Errorf("parse subscription rows: %w", err)
	}
	return newSubscription(ctx, id, rows, rows.body, rows.decoder, c.resubscribeWithBackoffFn(id)), nil
}

// ResubscribeContext resumes a subscription from a specific change ID.
func (c *Client) ResubscribeContext(ctx context.Context, id string, fromChange uint64) (*Subscription, error) {
	subURL := c.baseURL.JoinPath("/v1/subscriptions", id)
	q := subURL.Query()
	q.Set("from", strconv.FormatUint(fromChange, 10))
	subURL.RawQuery = q.Encode()

	req, err := http.NewRequestWithContext(ctx, "GET", subURL.String(), nil)
	if err != nil {
		return nil, fmt.Errorf("create resubscribe request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Accept", "application/json")

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("resubscribe: %w", err)
	}

	if resp.StatusCode != http.StatusOK {
		respBody, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
		resp.Body.Close()
		if err != nil {
			return nil, fmt.Errorf("read resubscribe response: %w", err)
		}
		return nil, fmt.Errorf("resubscribe: unexpected status %d: %s", resp.StatusCode, respBody)
	}

	return newSubscription(ctx, id, nil, resp.Body, nil, c.resubscribeWithBackoffFn(id)), nil
}

func (c *Client) resubscribeWithBackoffFn(id string) func(context.Context, uint64) (*Subscription, error) {
	if c.newResubBackoff == nil {
		return nil
	}
	return func(ctx context.Context, fromChange uint64) (*Subscription, error) {
		return backoff.RetryWithData(func() (*Subscription, error) {
			slog.Debug("Retrying resubscribe.", "id", id, "from_change", fromChange)
			return c.ResubscribeContext(ctx, id, fromChange)
		}, c.newResubBackoff())
	}
}
