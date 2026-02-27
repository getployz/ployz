package corrosion

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strconv"
	"strings"
)

type corrosionClient interface {
	exec(ctx context.Context, query string, args ...any) error
	query(ctx context.Context, query string, args ...any) ([][]json.RawMessage, error)
	subscribe(ctx context.Context, query string, args []any) (*subscriptionStream, error)
	resubscribe(ctx context.Context, id string, fromChange uint64) (*subscriptionStream, error)
}

type httpCorrosionClient struct {
	baseURL    string
	apiToken   string
	httpClient *http.Client
}

func newHTTPCorrosionClient(baseURL, apiToken string) *httpCorrosionClient {
	return &httpCorrosionClient{
		baseURL:    baseURL,
		apiToken:   strings.TrimSpace(apiToken),
		httpClient: http.DefaultClient,
	}
}

type rowEvent struct {
	RowID  uint64
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
	if err := json.Unmarshal(raw[0], &r.RowID); err != nil {
		return err
	}
	return json.Unmarshal(raw[1], &r.Values)
}

type changeEvent struct {
	Type     string
	RowID    uint64
	Values   []json.RawMessage
	ChangeID uint64
}

func (c *changeEvent) UnmarshalJSON(data []byte) error {
	var raw []json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	if len(raw) != 4 {
		return fmt.Errorf("invalid change event")
	}
	if err := json.Unmarshal(raw[0], &c.Type); err != nil {
		return err
	}
	if err := json.Unmarshal(raw[1], &c.RowID); err != nil {
		return err
	}
	if err := json.Unmarshal(raw[2], &c.Values); err != nil {
		return err
	}
	return json.Unmarshal(raw[3], &c.ChangeID)
}

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
	Columns []string     `json:"columns"`
	Row     *rowEvent    `json:"row"`
	EOQ     *endOfQuery  `json:"eoq"`
	Change  *changeEvent `json:"change"`
	Error   *string      `json:"error"`
}

type endOfQuery struct {
	Time     float64 `json:"time"`
	ChangeID *uint64 `json:"change_id"`
}

type subscriptionStream struct {
	ID      string
	Body    io.ReadCloser
	Decoder *json.Decoder
}

// newJSONRequest creates an HTTP request with JSON content headers and auth.
func (c *httpCorrosionClient) newJSONRequest(ctx context.Context, method, url string, body []byte) (*http.Request, error) {
	var reader io.Reader
	if body != nil {
		reader = bytes.NewReader(body)
	}
	req, err := http.NewRequestWithContext(ctx, method, url, reader)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Accept", "application/json")
	if c.apiToken != "" {
		req.Header.Set("Authorization", "Bearer "+c.apiToken)
	}
	return req, nil
}

// doJSON sends a JSON request and returns the response, returning an error for non-200 status.
func (c *httpCorrosionClient) doJSON(req *http.Request) (*http.Response, error) {
	httpClient := c.httpClient
	if httpClient == nil {
		httpClient = http.DefaultClient
	}
	resp, err := httpClient.Do(req)
	if err != nil {
		return nil, err
	}
	if resp.StatusCode != http.StatusOK {
		data, _ := io.ReadAll(io.LimitReader(resp.Body, 4096)) // best-effort, bounded
		resp.Body.Close()
		return nil, fmt.Errorf("status %d: %s", resp.StatusCode, strings.TrimSpace(string(data)))
	}
	return resp, nil
}

func (c *httpCorrosionClient) exec(ctx context.Context, query string, args ...any) error {
	body, err := json.Marshal([]statement{{Query: query, Params: args}})
	if err != nil {
		return fmt.Errorf("marshal corrosion transaction: %w", err)
	}

	req, err := c.newJSONRequest(ctx, http.MethodPost, c.baseURL+"/v1/transactions", body)
	if err != nil {
		return fmt.Errorf("create corrosion request: %w", err)
	}

	resp, err := c.doJSON(req)
	if err != nil {
		return fmt.Errorf("corrosion transaction: %w", err)
	}
	defer resp.Body.Close()

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

func (c *httpCorrosionClient) query(ctx context.Context, query string, args ...any) ([][]json.RawMessage, error) {
	body, err := json.Marshal(statement{Query: query, Params: args})
	if err != nil {
		return nil, fmt.Errorf("marshal corrosion query: %w", err)
	}

	req, err := c.newJSONRequest(ctx, http.MethodPost, c.baseURL+"/v1/queries", body)
	if err != nil {
		return nil, fmt.Errorf("create corrosion query request: %w", err)
	}

	resp, err := c.doJSON(req)
	if err != nil {
		return nil, fmt.Errorf("corrosion query: %w", err)
	}
	defer resp.Body.Close()

	dec := json.NewDecoder(resp.Body)
	var ev queryEvent
	if err := dec.Decode(&ev); err != nil {
		return nil, fmt.Errorf("decode corrosion columns event: %w", err)
	}
	if ev.Error != nil {
		return nil, fmt.Errorf("corrosion query error: %s", *ev.Error)
	}

	var rows [][]json.RawMessage
	for {
		ev = queryEvent{}
		if err := dec.Decode(&ev); err != nil {
			if errors.Is(err, io.EOF) {
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

func (c *httpCorrosionClient) subscribe(ctx context.Context, query string, args []any) (*subscriptionStream, error) {
	body, err := json.Marshal(statement{Query: query, Params: args})
	if err != nil {
		return nil, fmt.Errorf("marshal corrosion subscription: %w", err)
	}

	req, err := c.newJSONRequest(ctx, http.MethodPost, c.baseURL+"/v1/subscriptions", body)
	if err != nil {
		return nil, fmt.Errorf("create corrosion subscription request: %w", err)
	}

	resp, err := c.doJSON(req)
	if err != nil {
		return nil, fmt.Errorf("corrosion subscription: %w", err)
	}

	id := strings.TrimSpace(resp.Header.Get("corro-query-id"))
	if id == "" {
		resp.Body.Close()
		return nil, fmt.Errorf("corrosion subscription missing id header")
	}

	return &subscriptionStream{ID: id, Body: resp.Body, Decoder: json.NewDecoder(resp.Body)}, nil
}

func (c *httpCorrosionClient) resubscribe(ctx context.Context, id string, fromChange uint64) (*subscriptionStream, error) {
	url := c.baseURL + "/v1/subscriptions/" + id + "?from=" + strconv.FormatUint(fromChange, 10)
	req, err := c.newJSONRequest(ctx, http.MethodGet, url, nil)
	if err != nil {
		return nil, fmt.Errorf("create corrosion resubscribe request: %w", err)
	}

	resp, err := c.doJSON(req)
	if err != nil {
		return nil, fmt.Errorf("corrosion resubscribe: %w", err)
	}

	return &subscriptionStream{ID: id, Body: resp.Body, Decoder: json.NewDecoder(resp.Body)}, nil
}
