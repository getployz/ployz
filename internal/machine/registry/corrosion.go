package registry

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
)

type statement struct {
	Query  string `json:"query"`
	Params []any  `json:"params"`
}

type execResponse struct {
	Results []struct {
		Error *string `json:"error"`
	} `json:"results"`
}

type queryEvent struct {
	Columns []string         `json:"columns"`
	Row     *rowEvent        `json:"row"`
	EOQ     *json.RawMessage `json:"eoq"`
	Error   *string          `json:"error"`
}

type rowEvent struct {
	Values []json.RawMessage
}

func (r *rowEvent) UnmarshalJSON(data []byte) error {
	var raw []json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	if len(raw) != 2 {
		return fmt.Errorf("invalid row event")
	}
	return json.Unmarshal(raw[1], &r.Values)
}

func (s Store) exec(ctx context.Context, query string, args ...any) error {
	body, err := json.Marshal([]statement{{Query: query, Params: args}})
	if err != nil {
		return fmt.Errorf("marshal corrosion transaction: %w", err)
	}

	url := "http://" + s.apiAddr.String() + "/v1/transactions"
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewReader(body))
	if err != nil {
		return fmt.Errorf("create corrosion request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Accept", "application/json")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return fmt.Errorf("execute corrosion transaction: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		data, _ := io.ReadAll(resp.Body)
		return fmt.Errorf("corrosion transaction failed: status %d: %s", resp.StatusCode, strings.TrimSpace(string(data)))
	}

	var out execResponse
	if err := json.NewDecoder(resp.Body).Decode(&out); err != nil {
		return fmt.Errorf("decode corrosion transaction response: %w", err)
	}
	for _, r := range out.Results {
		if r.Error != nil {
			return fmt.Errorf("corrosion transaction error: %s", *r.Error)
		}
	}
	return nil
}

func (s Store) query(ctx context.Context, query string, args ...any) ([][]json.RawMessage, error) {
	body, err := json.Marshal(statement{Query: query, Params: args})
	if err != nil {
		return nil, fmt.Errorf("marshal corrosion query: %w", err)
	}

	url := "http://" + s.apiAddr.String() + "/v1/queries"
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewReader(body))
	if err != nil {
		return nil, fmt.Errorf("create corrosion query request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Accept", "application/json")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("execute corrosion query: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		data, _ := io.ReadAll(resp.Body)
		return nil, fmt.Errorf("corrosion query failed: status %d: %s", resp.StatusCode, strings.TrimSpace(string(data)))
	}

	dec := json.NewDecoder(resp.Body)
	var ev queryEvent
	if err := dec.Decode(&ev); err != nil {
		return nil, fmt.Errorf("decode corrosion columns event: %w", err)
	}
	if ev.Error != nil {
		return nil, fmt.Errorf("corrosion query error: %s", *ev.Error)
	}

	rows := make([][]json.RawMessage, 0)
	for {
		ev = queryEvent{}
		if err := dec.Decode(&ev); err != nil {
			if err == io.EOF {
				break
			}
			return nil, fmt.Errorf("decode corrosion query event: %w", err)
		}
		if ev.Error != nil {
			return nil, fmt.Errorf("corrosion query error: %s", *ev.Error)
		}
		if ev.Row != nil && len(ev.Row.Values) > 0 {
			rows = append(rows, ev.Row.Values)
		}
		if ev.EOQ != nil {
			break
		}
	}

	return rows, nil
}
