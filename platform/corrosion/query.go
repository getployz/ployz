package corrosion

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
)

// Statement is a parameterized SQL statement.
type Statement struct {
	Query  string `json:"query"`
	Params []any  `json:"params"`
}

// ExecResponse is the response from a transaction execution.
type ExecResponse struct {
	Results []ExecResult `json:"results"`
	Time    float64      `json:"time"`
	Version *uint        `json:"version"`
}

// ExecResult is the result of a single statement in a transaction.
type ExecResult struct {
	RowsAffected uint    `json:"rows_affected"`
	Time         float64 `json:"time"`
	Error        *string `json:"error"`
}

// ExecContext executes a single write statement and returns its result.
func (c *Client) ExecContext(ctx context.Context, query string, args ...any) (*ExecResult, error) {
	resp, err := c.ExecMultiContext(ctx, Statement{Query: query, Params: args})
	if err != nil {
		return nil, err
	}
	if len(resp.Results) == 0 {
		return nil, fmt.Errorf("exec %q: no results", query)
	}
	return &resp.Results[0], nil
}

// ExecMultiContext executes multiple statements in a single transaction.
func (c *Client) ExecMultiContext(ctx context.Context, statements ...Statement) (*ExecResponse, error) {
	body, err := json.Marshal(statements)
	if err != nil {
		return nil, fmt.Errorf("marshal statements: %w", err)
	}

	req, err := http.NewRequestWithContext(ctx, "POST",
		c.baseURL.JoinPath("/v1/transactions").String(), bytes.NewReader(body))
	if err != nil {
		return nil, fmt.Errorf("create request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Accept", "application/json")

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("exec transaction: %w", err)
	}
	defer resp.Body.Close()

	var execResp ExecResponse

	if resp.StatusCode == http.StatusOK {
		if err = json.NewDecoder(resp.Body).Decode(&execResp); err != nil {
			return nil, fmt.Errorf("decode exec response: %w", err)
		}
		var errs []error
		for _, result := range execResp.Results {
			if result.Error != nil {
				errs = append(errs, errors.New(*result.Error))
			}
		}
		return &execResp, errors.Join(errs...)
	}

	if resp.StatusCode == http.StatusInternalServerError {
		respBody, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
		if err != nil {
			return nil, fmt.Errorf("read error response: %w", err)
		}
		if err = json.Unmarshal(respBody, &execResp); err != nil {
			return nil, fmt.Errorf("exec transaction: server error: %s", respBody)
		}
		if len(execResp.Results) > 0 && execResp.Results[0].Error != nil {
			return nil, errors.New(*execResp.Results[0].Error)
		}
		return nil, fmt.Errorf("exec transaction: server error: %s", respBody)
	}

	respBody, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	if err != nil {
		return nil, fmt.Errorf("read response: %w", err)
	}
	return nil, fmt.Errorf("exec transaction: unexpected status %d: %s", resp.StatusCode, respBody)
}

// QueryEvent is a single event in a streaming query response.
type QueryEvent struct {
	Columns []string     `json:"columns"`
	Row     *RowEvent    `json:"row"`
	EOQ     *EndOfQuery  `json:"eoq"`
	Change  *ChangeEvent `json:"change"`
	Error   *string      `json:"error"`
}

// EndOfQuery marks the end of the row stream.
type EndOfQuery struct {
	Time     float64 `json:"time"`
	ChangeID *uint64 `json:"change_id"`
}

// RowEvent is a single row in a query result: [rowid, [values...]].
type RowEvent struct {
	RowID  uint64
	Values []json.RawMessage
}

func (re *RowEvent) UnmarshalJSON(data []byte) error {
	var raw [2]json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil {
		return fmt.Errorf("invalid row event: %w", err)
	}
	if err := json.Unmarshal(raw[0], &re.RowID); err != nil {
		return fmt.Errorf("invalid row event rowid: %w", err)
	}
	if err := json.Unmarshal(raw[1], &re.Values); err != nil {
		return fmt.Errorf("invalid row event values: %w", err)
	}
	return nil
}

func (re *RowEvent) MarshalJSON() ([]byte, error) {
	return json.Marshal([]any{re.RowID, re.Values})
}

// QueryContext executes a SELECT query and returns streaming rows.
func (c *Client) QueryContext(ctx context.Context, query string, args ...any) (*Rows, error) {
	body, err := json.Marshal(Statement{Query: query, Params: args})
	if err != nil {
		return nil, fmt.Errorf("marshal query: %w", err)
	}

	req, err := http.NewRequestWithContext(ctx, "POST",
		c.baseURL.JoinPath("/v1/queries").String(), bytes.NewReader(body))
	if err != nil {
		return nil, fmt.Errorf("create request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Accept", "application/json")

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("query: %w", err)
	}

	if resp.StatusCode != http.StatusOK {
		respBody, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
		resp.Body.Close()
		if err != nil {
			return nil, fmt.Errorf("read response: %w", err)
		}
		return nil, fmt.Errorf("query: unexpected status %d: %s", resp.StatusCode, respBody)
	}

	rows, err := newRows(ctx, resp.Body, true)
	if err != nil {
		resp.Body.Close()
		return nil, fmt.Errorf("parse query response: %w", err)
	}
	return rows, nil
}

// Rows iterates over query results. Call Next to advance, Scan to read values.
type Rows struct {
	ctx        context.Context
	body       io.ReadCloser
	decoder    *json.Decoder
	eoq        *EndOfQuery
	closeOnEOQ bool

	columns []string
	row     RowEvent
	err     error
}

func newRows(ctx context.Context, body io.ReadCloser, closeOnEOQ bool) (*Rows, error) {
	select {
	case <-ctx.Done():
		return nil, ctx.Err()
	default:
	}

	decoder := json.NewDecoder(body)
	var e QueryEvent
	if err := decoder.Decode(&e); err != nil {
		return nil, fmt.Errorf("decode columns event: %w", err)
	}
	if e.Columns == nil {
		return nil, fmt.Errorf("expected columns event, got: %+v", e)
	}

	return &Rows{
		ctx:        ctx,
		body:       body,
		decoder:    decoder,
		closeOnEOQ: closeOnEOQ,
		columns:    e.Columns,
	}, nil
}

// Columns returns the column names.
func (rs *Rows) Columns() []string {
	return rs.columns
}

// Next advances to the next row. Returns false when done or on error.
func (rs *Rows) Next() bool {
	select {
	case <-rs.ctx.Done():
		rs.err = rs.ctx.Err()
		_ = rs.Close() // best-effort
		return false
	default:
	}

	var e QueryEvent
	if err := rs.decoder.Decode(&e); err != nil {
		rs.err = fmt.Errorf("decode row event: %w", err)
		_ = rs.Close() // best-effort
		return false
	}
	if e.Error != nil {
		rs.err = fmt.Errorf("query error: %s", *e.Error)
		_ = rs.Close() // best-effort
		return false
	}

	if e.Row != nil {
		if len(e.Row.Values) != len(rs.columns) {
			rs.err = fmt.Errorf("column count mismatch: expected %d, got %d", len(rs.columns), len(e.Row.Values))
			_ = rs.Close() // best-effort
			return false
		}
		rs.row = *e.Row
		return true
	}
	if e.EOQ != nil {
		rs.eoq = e.EOQ
		if rs.closeOnEOQ {
			_ = rs.Close() // best-effort
		}
		return false
	}

	rs.err = fmt.Errorf("unexpected query event: %+v", e)
	_ = rs.Close() // best-effort
	return false
}

// Err returns the error encountered during iteration, if any.
func (rs *Rows) Err() error {
	return rs.err
}

// Scan unmarshals the current row's column values into dest.
func (rs *Rows) Scan(dest ...any) error {
	if rs.err != nil {
		return rs.err
	}
	if len(dest) != len(rs.columns) {
		return fmt.Errorf("scan: expected %d values, got %d", len(rs.columns), len(dest))
	}
	for i, v := range rs.row.Values {
		if err := json.Unmarshal(v, dest[i]); err != nil {
			return fmt.Errorf("scan column %d: %w", i, err)
		}
	}
	return nil
}

// Time returns the server-side query execution time. Only available after
// all rows have been consumed.
func (rs *Rows) Time() (float64, error) {
	if rs.eoq == nil {
		if rs.err != nil {
			return 0, fmt.Errorf("query time unavailable: %w", rs.err)
		}
		return 0, errors.New("query time unavailable: rows not fully consumed")
	}
	return rs.eoq.Time, nil
}

// Close releases the underlying response body.
func (rs *Rows) Close() error {
	return rs.body.Close()
}
