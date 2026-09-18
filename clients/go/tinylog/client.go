package tinylog

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

type Level string

const (
	Debug Level = "debug"
	Info  Level = "info"
	Warn  Level = "warn"
	Error Level = "error"
	Fatal Level = "fatal"
)

type Status string

const (
	StatusIdle    Status = "idle"
	StatusSending Status = "sending"
	StatusOK      Status = "ok"
	StatusFailed  Status = "failed"
	StatusUnable  Status = "unable"
)

type Entry struct {
	App       string         `json:"app"`
	Level     Level          `json:"level"`
	Message   string         `json:"message"`
	Source    string         `json:"source,omitempty"`
	Timestamp string         `json:"timestamp,omitempty"`
	Meta      map[string]any `json:"meta,omitempty"`
}

type Config struct {
	BaseURL    string
	APIKey     string
	App        string
	Source     string
	HTTPClient *http.Client
	MaxRetries int
	OnUnable   func(err error, entries []Entry)
}

type Client struct {
	cfg    Config
	http   *http.Client
	status atomic.Value
	last   atomic.Value
	mu     sync.Mutex
}

func New(cfg Config) *Client {
	if cfg.MaxRetries <= 0 {
		cfg.MaxRetries = 3
	}
	if cfg.Source == "" {
		cfg.Source = "go"
	}
	hc := cfg.HTTPClient
	if hc == nil {
		hc = &http.Client{Timeout: 10 * time.Second}
	}
	c := &Client{cfg: cfg, http: hc}
	c.status.Store(StatusIdle)
	c.last.Store("")
	return c
}

func (c *Client) Status() Status {
	if v, ok := c.status.Load().(Status); ok {
		return v
	}
	return StatusIdle
}

func (c *Client) LastError() string {
	if v, ok := c.last.Load().(string); ok {
		return v
	}
	return ""
}

func (c *Client) Log(ctx context.Context, level Level, message string, meta map[string]any) error {
	return c.Send(ctx, Entry{
		App:       c.cfg.App,
		Level:     level,
		Message:   message,
		Source:    c.cfg.Source,
		Timestamp: time.Now().UTC().Format(time.RFC3339Nano),
		Meta:      meta,
	})
}

func (c *Client) Send(ctx context.Context, entry Entry) error {
	return c.SendBatch(ctx, []Entry{entry})
}

func (c *Client) SendBatch(ctx context.Context, entries []Entry) error {
	c.mu.Lock()
	defer c.mu.Unlock()

	if len(entries) == 0 {
		return nil
	}
	for i := range entries {
		if entries[i].App == "" {
			entries[i].App = c.cfg.App
		}
		if entries[i].Source == "" {
			entries[i].Source = c.cfg.Source
		}
		if entries[i].Timestamp == "" {
			entries[i].Timestamp = time.Now().UTC().Format(time.RFC3339Nano)
		}
		entries[i].Level = Level(strings.ToLower(string(entries[i].Level)))
	}

	c.set(StatusSending, "")
	var last error
	for attempt := 0; attempt < c.cfg.MaxRetries; attempt++ {
		last = c.post(ctx, entries)
		if last == nil {
			c.set(StatusOK, "")
			return nil
		}
		if isNonRetryable(last) {
			break
		}
		select {
		case <-ctx.Done():
			last = ctx.Err()
			c.fail(last, entries)
			return last
		case <-time.After(time.Duration(200*(1<<attempt)) * time.Millisecond):
		}
	}
	c.fail(last, entries)
	return last
}

func (c *Client) post(ctx context.Context, entries []Entry) error {
	var (
		url  string
		body []byte
		err  error
	)
	if len(entries) == 1 {
		url = strings.TrimRight(c.cfg.BaseURL, "/") + "/api/v1/logs"
		body, err = json.Marshal(entries[0])
	} else {
		url = strings.TrimRight(c.cfg.BaseURL, "/") + "/api/v1/logs/batch"
		body, err = json.Marshal(map[string]any{"logs": entries})
	}
	if err != nil {
		return err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewReader(body))
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+c.cfg.APIKey)
	req.Header.Set("Content-Type", "application/json")
	res, err := c.http.Do(req)
	if err != nil {
		return fmt.Errorf("transport: %w", err)
	}
	defer res.Body.Close()
	b, _ := io.ReadAll(io.LimitReader(res.Body, 4096))
	if res.StatusCode >= 200 && res.StatusCode < 300 {
		return nil
	}
	return &httpError{code: res.StatusCode, body: string(b)}
}

func (c *Client) set(s Status, errMsg string) {
	c.status.Store(s)
	c.last.Store(errMsg)
}

func (c *Client) fail(err error, entries []Entry) {
	msg := ""
	if err != nil {
		msg = err.Error()
	}
	c.set(StatusUnable, msg)
	if c.cfg.OnUnable != nil {
		c.cfg.OnUnable(err, entries)
	}
}

type httpError struct {
	code int
	body string
}

func (e *httpError) Error() string {
	return fmt.Sprintf("tiny-log http %d: %s", e.code, e.body)
}

func isNonRetryable(err error) bool {
	var he *httpError
	if errors.As(err, &he) {
		return he.code == 400 || he.code == 401 || he.code == 403 || he.code == 413
	}
	return false
}
