package corrosion

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strconv"
)

// Health is the response from the Corrosion health endpoint.
type Health struct {
	Gaps      int     `json:"gaps"`
	Members   int     `json:"members"`
	P99Lag    float64 `json:"p99_lag"`
	QueueSize int     `json:"queue_size"`
}

// HealthThresholds are the thresholds passed to the health endpoint.
// Zero values mean "don't gate on this field".
type HealthThresholds struct {
	Gaps      int
	P99Lag    float64
	QueueSize int
}

// HealthContext checks Corrosion's health against the given thresholds.
// Returns the health status and whether the thresholds were met (HTTP 200 vs 503).
func (c *Client) HealthContext(ctx context.Context, thresholds HealthThresholds) (Health, bool, error) {
	healthURL := c.baseURL.JoinPath("/v1/health")
	q := healthURL.Query()
	q.Set("gaps", strconv.Itoa(thresholds.Gaps))
	if thresholds.P99Lag > 0 {
		q.Set("p99_lag", strconv.FormatFloat(thresholds.P99Lag, 'f', -1, 64))
	}
	q.Set("queue_size", strconv.Itoa(thresholds.QueueSize))
	healthURL.RawQuery = q.Encode()

	req, err := http.NewRequestWithContext(ctx, "GET", healthURL.String(), nil)
	if err != nil {
		return Health{}, false, fmt.Errorf("create health request: %w", err)
	}
	req.Header.Set("Accept", "application/json")

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return Health{}, false, fmt.Errorf("health check: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK && resp.StatusCode != http.StatusServiceUnavailable {
		body, _ := io.ReadAll(io.LimitReader(resp.Body, maxErrorBodySize))
		return Health{}, false, fmt.Errorf("health check: unexpected status %d: %s", resp.StatusCode, body)
	}

	var envelope struct {
		Response Health `json:"response"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&envelope); err != nil {
		return Health{}, false, fmt.Errorf("health check: decode response: %w", err)
	}

	return envelope.Response, resp.StatusCode == http.StatusOK, nil
}
