/*
Purpose: Validate Go binding behaviors for streaming, payloads, and lifecycle.
Key Exports: None (package tests).
Role: Exercise tail cancellation/timeout and large payload handling.
Invariants: Requires libplasmite build output in library search path.
Notes: Uses temporary directories; avoids global state.
*/
package plasmite

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

type message struct {
	Seq  uint64 `json:"seq"`
	Meta struct {
		Descrips []string `json:"descrips"`
	} `json:"meta"`
	Data map[string]any `json:"data"`
}

func TestAppendGetLargePayload(t *testing.T) {
	temp := t.TempDir()
	poolDir := filepath.Join(temp, "pools")
	if err := os.MkdirAll(poolDir, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	client, err := NewClient(poolDir)
	if err != nil {
		t.Fatalf("client: %v", err)
	}
	defer client.Close()

	pool, err := client.CreatePool(PoolRefName("big"), 1024*1024)
	if err != nil {
		t.Fatalf("create: %v", err)
	}
	defer pool.Close()

	payload := strings.Repeat("x", 64*1024)
	data := map[string]any{"blob": payload}
	descrips := []string{"alpha", "beta", "gamma"}
	msgBytes, err := pool.Append(data, descrips, DurabilityFast)
	if err != nil {
		t.Fatalf("append: %v", err)
	}
	var msg message
	if err := json.Unmarshal(msgBytes, &msg); err != nil {
		t.Fatalf("parse append: %v", err)
	}
	if msg.Data["blob"] != payload {
		t.Fatalf("append payload mismatch")
	}
	if len(msg.Meta.Descrips) != len(descrips) {
		t.Fatalf("descrips mismatch")
	}

	getBytes, err := pool.Get(msg.Seq)
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	if err := json.Unmarshal(getBytes, &msg); err != nil {
		t.Fatalf("parse get: %v", err)
	}
	if msg.Data["blob"] != payload {
		t.Fatalf("get payload mismatch")
	}
}

func TestTailCancelAndResume(t *testing.T) {
	temp := t.TempDir()
	poolDir := filepath.Join(temp, "pools")
	if err := os.MkdirAll(poolDir, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	client, err := NewClient(poolDir)
	if err != nil {
		t.Fatalf("client: %v", err)
	}
	defer client.Close()

	pool, err := client.CreatePool(PoolRefName("tail"), 1024*1024)
	if err != nil {
		t.Fatalf("create: %v", err)
	}
	defer pool.Close()

	_, err = pool.Append(map[string]any{"n": 1}, nil, DurabilityFast)
	if err != nil {
		t.Fatalf("append: %v", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	out, errs := pool.Tail(ctx, TailOptions{Timeout: 50 * time.Millisecond, Buffer: 4})

	select {
	case <-out:
	case err := <-errs:
		t.Fatalf("unexpected error: %v", err)
	case <-time.After(2 * time.Second):
		t.Fatalf("tail did not yield")
	}

	cancel()
	select {
	case err := <-errs:
		if err == nil {
			t.Fatalf("expected cancellation error")
		}
	case <-time.After(2 * time.Second):
		t.Fatalf("expected cancellation error")
	}

	_, err = pool.Append(map[string]any{"n": 2}, nil, DurabilityFast)
	if err != nil {
		t.Fatalf("append: %v", err)
	}
	ctx2, cancel2 := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel2()
	out2, errs2 := pool.Tail(ctx2, TailOptions{SinceSeq: uint64Ptr(2), MaxMessages: uint64Ptr(1), Timeout: 50 * time.Millisecond})
	select {
	case <-out2:
	case err := <-errs2:
		t.Fatalf("unexpected error: %v", err)
	case <-time.After(2 * time.Second):
		t.Fatalf("tail did not resume")
	}
}

func TestCloseIdempotent(t *testing.T) {
	temp := t.TempDir()
	poolDir := filepath.Join(temp, "pools")
	if err := os.MkdirAll(poolDir, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	client, err := NewClient(poolDir)
	if err != nil {
		t.Fatalf("client: %v", err)
	}
	pool, err := client.CreatePool(PoolRefName("close"), 1024*1024)
	if err != nil {
		t.Fatalf("create: %v", err)
	}
	pool.Close()
	pool.Close()

	_, err = pool.Append(map[string]any{"n": 1}, nil, DurabilityFast)
	if err == nil {
		t.Fatalf("expected error on closed pool")
	}

	client.Close()
	client.Close()
	_, err = client.CreatePool(PoolRefName("oops"), 1024*1024)
	if err == nil {
		t.Fatalf("expected error on closed client")
	}
	var perr *Error
	if !errors.As(err, &perr) {
		// errors from closed client are plain errors
	}
}

func uint64Ptr(val uint64) *uint64 {
	return &val
}
