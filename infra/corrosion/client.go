package corrosion

import (
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"net/netip"
	"net/url"
	"time"

	"github.com/cenkalti/backoff/v4"
	"golang.org/x/net/http2"
)

const (
	// http2ConnectTimeout is the maximum time to wait for a connection.
	http2ConnectTimeout = 3 * time.Second
	// http2MaxRetryTime is the maximum time to retry a request.
	http2MaxRetryTime = 10 * time.Second
	// resubscribeMaxRetryTime is the maximum time to retry resubscribing.
	resubscribeMaxRetryTime = 60 * time.Second
)

// Client is an HTTP client for the Corrosion API.
type Client struct {
	baseURL         *url.URL
	httpClient      *http.Client
	newResubBackoff func() backoff.BackOff
}

// NewClient creates a Corrosion API client with HTTP/2 transport and exponential
// backoff on network errors.
func NewClient(addr netip.AddrPort, opts ...ClientOption) (*Client, error) {
	baseURL, err := url.Parse(fmt.Sprintf("http://%s", addr))
	if err != nil {
		return nil, fmt.Errorf("parse corrosion URL: %w", err)
	}

	c := &Client{
		baseURL: baseURL,
		httpClient: &http.Client{
			Transport: &retryRoundTripper{
				base: &http2.Transport{
					AllowHTTP: true,
					DialTLSContext: func(ctx context.Context, network, addr string, _ *tls.Config) (net.Conn, error) {
						return (&net.Dialer{Timeout: http2ConnectTimeout}).DialContext(ctx, network, addr)
					},
				},
				newBackoff: func() backoff.BackOff {
					return backoff.NewExponentialBackOff(
						backoff.WithInitialInterval(100*time.Millisecond),
						backoff.WithMaxInterval(1*time.Second),
						backoff.WithMaxElapsedTime(http2MaxRetryTime),
					)
				},
			},
		},
		newResubBackoff: func() backoff.BackOff {
			return backoff.NewExponentialBackOff(
				backoff.WithInitialInterval(100*time.Millisecond),
				backoff.WithMaxInterval(1*time.Second),
				backoff.WithMaxElapsedTime(resubscribeMaxRetryTime),
			)
		},
	}

	for _, opt := range opts {
		opt(c)
	}
	return c, nil
}

// ClientOption configures a Client.
type ClientOption func(*Client)

// WithHTTPClient sets a custom HTTP client.
func WithHTTPClient(client *http.Client) ClientOption {
	return func(c *Client) {
		c.httpClient = client
	}
}

// WithResubscribeBackoff sets the backoff policy for resubscribing.
// Pass nil to disable automatic resubscription.
func WithResubscribeBackoff(newBackoff func() backoff.BackOff) ClientOption {
	return func(c *Client) {
		c.newResubBackoff = newBackoff
	}
}

// retryRoundTripper retries requests on transient network errors.
type retryRoundTripper struct {
	base       http.RoundTripper
	newBackoff func() backoff.BackOff
}

func (rt *retryRoundTripper) RoundTrip(req *http.Request) (*http.Response, error) {
	attempt := func() (*http.Response, error) {
		resp, err := rt.base.RoundTrip(req)
		if err != nil {
			var opErr *net.OpError
			if errors.As(err, &opErr) {
				slog.Debug("Retrying corrosion request due to network error.", "error", err)
				return nil, err
			}
			return nil, backoff.Permanent(err)
		}
		return resp, nil
	}
	boff := backoff.WithContext(rt.newBackoff(), req.Context())
	return backoff.RetryWithData(attempt, boff)
}
