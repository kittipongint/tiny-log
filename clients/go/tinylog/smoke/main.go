package main

import (
	"context"
	"fmt"
	"os"
	"time"

	"github.com/tiny-log/clients/go/tinylog"
)

func main() {
	base := env("TINY_LOG_URL", "http://127.0.0.1:8080")
	key := env("TINY_LOG_API_KEY", "dev-api-key")
	unable := false
	c := tinylog.New(tinylog.Config{
		BaseURL: base,
		APIKey:  key,
		App:     "go-smoke",
		Source:  "go-smoke",
		OnUnable: func(err error, _ []tinylog.Entry) {
			unable = true
			fmt.Println("onUnable:", err)
		},
	})
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()

	if err := c.Log(ctx, tinylog.Info, "go helper smoke", map[string]any{"ok": true}); err != nil {
		fmt.Println("FAIL send:", err, "status=", c.Status())
		os.Exit(1)
	}
	if c.Status() != tinylog.StatusOK {
		fmt.Println("FAIL status:", c.Status())
		os.Exit(1)
	}
	if unable {
		fmt.Println("FAIL unexpected onUnable")
		os.Exit(1)
	}

	// bad key should become unable without hanging forever
	bad := tinylog.New(tinylog.Config{
		BaseURL:    base,
		APIKey:     "wrong-key",
		App:        "go-smoke",
		MaxRetries: 1,
	})
	err := bad.Log(ctx, tinylog.Info, "should fail", nil)
	if err == nil || bad.Status() != tinylog.StatusUnable {
		fmt.Println("FAIL expected unable, got", bad.Status(), err)
		os.Exit(1)
	}
	fmt.Println("OK go helper status=", c.Status())
}

func env(k, def string) string {
	if v := os.Getenv(k); v != "" {
		return v
	}
	return def
}
