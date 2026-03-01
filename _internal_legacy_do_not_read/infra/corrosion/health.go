package corrosion

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"math"
	"net/http"
	"net/netip"
	"strings"
	"time"
)

const (
	corrosionHealthPath         = "/v1/health?gaps=0&p99_lag=5.0&queue_size=100"
	corrosionHealthBodyMaxBytes = 8 * 1024
	corrosionHealthTimeout      = 2 * time.Second
)

var (
	maxIntValue      = int(^uint(0) >> 1)
	maxUint64AsFloat = float64(^uint64(0))
)

func ProbeHealth(ctx context.Context, apiAddr netip.AddrPort, apiToken string, expectedMembers int) HealthPhase {
	url := "http://" + apiAddr.String() + corrosionHealthPath
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return HealthUnreachable
	}
	req.Header.Set("Accept", "application/json")
	if token := strings.TrimSpace(apiToken); token != "" {
		req.Header.Set("Authorization", "Bearer "+token)
	}

	resp, err := (&http.Client{Timeout: corrosionHealthTimeout}).Do(req)
	if err != nil {
		return HealthUnreachable
	}
	defer resp.Body.Close()

	thresholdsMet := false
	switch resp.StatusCode {
	case http.StatusOK:
		thresholdsMet = true
	case http.StatusServiceUnavailable:
		thresholdsMet = false
	default:
		return HealthUnreachable
	}

	body, err := io.ReadAll(io.LimitReader(resp.Body, corrosionHealthBodyMaxBytes))
	if err != nil {
		return HealthUnreachable
	}

	sample, ok := decodeHealthSample(body, thresholdsMet)
	if !ok {
		return HealthUnreachable
	}
	return ClassifyHealth(sample, expectedMembers)
}

func decodeHealthSample(body []byte, thresholdsMet bool) (HealthSample, bool) {
	var payload struct {
		Response *struct {
			Gaps      json.Number `json:"gaps"`
			Members   json.Number `json:"members"`
			QueueSize json.Number `json:"queue_size"`
		} `json:"response"`
	}

	dec := json.NewDecoder(bytes.NewReader(body))
	dec.UseNumber()
	if err := dec.Decode(&payload); err != nil {
		return HealthSample{}, false
	}
	if payload.Response == nil {
		return HealthSample{}, false
	}

	members, ok := parseNonNegativeInt(payload.Response.Members)
	if !ok {
		return HealthSample{}, false
	}
	gaps, ok := parseNonNegativeUint(payload.Response.Gaps)
	if !ok {
		return HealthSample{}, false
	}
	queueSize, ok := parseNonNegativeUint(payload.Response.QueueSize)
	if !ok {
		return HealthSample{}, false
	}

	return HealthSample{
		Reachable:     true,
		ThresholdsMet: thresholdsMet,
		Members:       members,
		Gaps:          gaps,
		QueueSize:     queueSize,
	}, true
}

func parseNonNegativeInt(raw json.Number) (int, bool) {
	text := strings.TrimSpace(raw.String())
	if text == "" {
		return 0, false
	}
	i64, err := raw.Int64()
	if err == nil {
		if i64 < 0 || i64 > int64(maxIntValue) {
			return 0, false
		}
		return int(i64), true
	}
	f64, err := raw.Float64()
	if err != nil {
		return 0, false
	}
	if f64 < 0 || math.Trunc(f64) != f64 || f64 > float64(maxIntValue) {
		return 0, false
	}
	return int(f64), true
}

func parseNonNegativeUint(raw json.Number) (uint64, bool) {
	text := strings.TrimSpace(raw.String())
	if text == "" {
		return 0, false
	}
	i64, err := raw.Int64()
	if err == nil {
		if i64 < 0 {
			return 0, false
		}
		return uint64(i64), true
	}
	f64, err := raw.Float64()
	if err != nil {
		return 0, false
	}
	if f64 < 0 || math.Trunc(f64) != f64 || f64 > maxUint64AsFloat {
		return 0, false
	}
	return uint64(f64), true
}
