/*
Purpose: Exercise the Go cookbook snippet path in smoke automation.
Key Exports: main (command entrypoint).
Role: Validate local client.pool(), typed append return fields, and not-found handling.
Invariants: Runs against local pool directories only; no network usage.
Invariants: Produces deterministic JSON output for shell assertions.
Notes: Designed for scripts/cookbook_smoke.sh integration.
*/
package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"

	plasmite "github.com/sandover/plasmite/bindings/go/local"
)

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run() error {
	if len(os.Args) != 2 {
		return errors.New("usage: cookbook-smoke-fixture <pool-dir>")
	}
	poolDir := os.Args[1]
	if err := os.RemoveAll(poolDir); err != nil {
		return fmt.Errorf("clear pool dir: %w", err)
	}
	if err := os.MkdirAll(poolDir, 0o755); err != nil {
		return fmt.Errorf("create pool dir: %w", err)
	}

	client, err := plasmite.NewClient(poolDir)
	if err != nil {
		return fmt.Errorf("new client: %w", err)
	}
	defer client.Close()

	pool, err := client.Pool(plasmite.PoolRefName("cookbook-smoke"), 0)
	if err != nil {
		return fmt.Errorf("pool open/create: %w", err)
	}
	defer pool.Close()

	msg, err := pool.Append(
		map[string]any{"task": "resize", "id": 1},
		[]string{"cookbook"},
		plasmite.WithDurability(plasmite.DurabilityFast),
	)
	if err != nil {
		return fmt.Errorf("append: %w", err)
	}
	if msg.Seq < 1 {
		return errors.New("expected positive seq")
	}
	if len(msg.Meta.Tags) != 1 || msg.Meta.Tags[0] != "cookbook" {
		return fmt.Errorf("unexpected tags: %#v", msg.Meta.Tags)
	}
	var payload map[string]any
	if err := json.Unmarshal(msg.Data, &payload); err != nil {
		return fmt.Errorf("decode payload: %w", err)
	}
	if payload["task"] != "resize" {
		return fmt.Errorf("unexpected data: %#v", payload)
	}

	_, err = client.OpenPool(plasmite.PoolRefName("missing-cookbook-smoke-pool"))
	if err == nil {
		return errors.New("expected not-found error")
	}
	var perr *plasmite.Error
	if !errors.As(err, &perr) || perr.Kind != plasmite.ErrorNotFound {
		return fmt.Errorf("unexpected error for missing pool: %w", err)
	}

	out, err := json.Marshal(map[string]any{
		"seq":  msg.Seq,
		"tags": msg.Meta.Tags,
		"data": payload,
	})
	if err != nil {
		return fmt.Errorf("encode output: %w", err)
	}
	fmt.Println(string(out))
	return nil
}
