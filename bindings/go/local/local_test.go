/*
Purpose: Validate Go binding behaviors for streaming, payloads, and lifecycle.
Key Exports: None (package tests).
Role: Exercise tail cancellation/timeout and large payload handling.
Invariants: Requires libplasmite build output in library search path.
Notes: Uses temporary directories; avoids global state.
*/
package local

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/sandover/plasmite/bindings/go/api"
)

const testPoolSizeBytes uint64 = 1024 * 1024

func newTestClient(t *testing.T) *Client {
	t.Helper()
	poolDir := filepath.Join(t.TempDir(), "pools")
	if err := os.MkdirAll(poolDir, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	client, err := NewClient(poolDir)
	if err != nil {
		t.Fatalf("client: %v", err)
	}
	t.Cleanup(func() {
		client.Close()
	})
	return client
}

func newTestPool(t *testing.T, client *Client, name string) api.Pool {
	t.Helper()
	pool, err := client.CreatePool(PoolRefName(name), testPoolSizeBytes)
	if err != nil {
		t.Fatalf("create: %v", err)
	}
	t.Cleanup(func() {
		pool.Close()
	})
	return pool
}

func TestAppendGetLargePayload(t *testing.T) {
	client := newTestClient(t)
	pool := newTestPool(t, client, "big")

	payload := strings.Repeat("x", 64*1024)
	data := map[string]any{"blob": payload}
	tags := []string{"alpha", "beta", "gamma"}
	msg, err := pool.Append(data, tags, WithDurability(DurabilityFast))
	if err != nil {
		t.Fatalf("append: %v", err)
	}
	decoded, err := decodeObject(msg.Data)
	if err != nil {
		t.Fatalf("decode append: %v", err)
	}
	if decoded["blob"] != payload {
		t.Fatalf("append payload mismatch")
	}
	if len(msg.Meta.Tags) != len(tags) {
		t.Fatalf("tags mismatch")
	}

	getMsg, err := pool.Get(msg.Seq)
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	decodedGet, err := decodeObject(getMsg.Data)
	if err != nil {
		t.Fatalf("decode get: %v", err)
	}
	if decodedGet["blob"] != payload {
		t.Fatalf("get payload mismatch")
	}
}

func TestClientPoolCreateThenReopen(t *testing.T) {
	client := newTestClient(t)

	first, err := client.Pool(PoolRefName("work"), testPoolSizeBytes)
	if err != nil {
		t.Fatalf("pool create/open first: %v", err)
	}
	defer first.Close()

	second, err := client.Pool(PoolRefName("work"), 2*testPoolSizeBytes)
	if err != nil {
		t.Fatalf("pool create/open second: %v", err)
	}
	defer second.Close()

	msg, err := first.Append(map[string]any{"kind": "created"}, []string{"alpha"}, WithDurability(DurabilityFast))
	if err != nil {
		t.Fatalf("append: %v", err)
	}

	getMsg, err := second.Get(msg.Seq)
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	decoded, err := decodeObject(getMsg.Data)
	if err != nil {
		t.Fatalf("decode get: %v", err)
	}
	if decoded["kind"] != "created" {
		t.Fatalf("expected created kind")
	}
}

func TestTailCancelAndResume(t *testing.T) {
	client := newTestClient(t)
	pool := newTestPool(t, client, "tail")

	_, err := pool.Append(map[string]any{"n": 1}, nil, WithDurability(DurabilityFast))
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

	_, err = pool.Append(map[string]any{"n": 2}, nil, WithDurability(DurabilityFast))
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

func TestTailFiltersByTags(t *testing.T) {
	client := newTestClient(t)
	pool := newTestPool(t, client, "tagged")

	if _, err := pool.Append(map[string]any{"kind": "drop"}, []string{"drop"}, WithDurability(DurabilityFast)); err != nil {
		t.Fatalf("append drop: %v", err)
	}
	if _, err := pool.Append(map[string]any{"kind": "keep"}, []string{"keep"}, WithDurability(DurabilityFast)); err != nil {
		t.Fatalf("append keep: %v", err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	out, errs := pool.Tail(ctx, TailOptions{
		Tags:        []string{"keep"},
		MaxMessages: uint64Ptr(1),
		Timeout:     50 * time.Millisecond,
	})

	select {
	case msg := <-out:
		decoded, err := decodeObject(msg.Data)
		if err != nil {
			t.Fatalf("decode: %v", err)
		}
		if decoded["kind"] != "keep" {
			t.Fatalf("expected keep message, got %v", decoded["kind"])
		}
	case err := <-errs:
		t.Fatalf("unexpected error: %v", err)
	case <-time.After(2 * time.Second):
		t.Fatalf("tail did not yield filtered message")
	}
}

func TestCloseIdempotent(t *testing.T) {
	client := newTestClient(t)
	pool := newTestPool(t, client, "close")
	pool.Close()
	pool.Close()

	_, err := pool.Append(map[string]any{"n": 1}, nil, WithDurability(DurabilityFast))
	if err == nil {
		t.Fatalf("expected error on closed pool")
	}
	if !errors.Is(err, ErrClosed) {
		t.Fatalf("expected ErrClosed, got %v", err)
	}

	client.Close()
	client.Close()
	_, err = client.CreatePool(PoolRefName("oops"), testPoolSizeBytes)
	if err == nil {
		t.Fatalf("expected error on closed client")
	}
	if !errors.Is(err, ErrClosed) {
		t.Fatalf("expected ErrClosed, got %v", err)
	}
}

func TestArgumentErrorsUseSentinel(t *testing.T) {
	if _, err := NewClient(""); !errors.Is(err, ErrInvalidArgument) {
		t.Fatalf("expected ErrInvalidArgument for empty poolDir, got %v", err)
	}

	client := newTestClient(t)

	if _, err := client.CreatePool(PoolRefName(""), testPoolSizeBytes); !errors.Is(err, ErrInvalidArgument) {
		t.Fatalf("expected ErrInvalidArgument for empty pool ref, got %v", err)
	}

	pool := newTestPool(t, client, "args")

	if _, err := pool.AppendJSON(nil, nil, DurabilityFast); !errors.Is(err, ErrInvalidArgument) {
		t.Fatalf("expected ErrInvalidArgument for empty payload, got %v", err)
	}
}

func TestLite3AppendGetTail(t *testing.T) {
	client := newTestClient(t)
	pool := newTestPool(t, client, "lite3")

	msg, err := pool.Append(map[string]any{"x": 1}, []string{"alpha"}, WithDurability(DurabilityFast))
	if err != nil {
		t.Fatalf("append json: %v", err)
	}

	seedFrame, err := pool.GetLite3(msg.Seq)
	if err != nil {
		t.Fatalf("get lite3: %v", err)
	}
	if len(seedFrame.Payload) == 0 {
		t.Fatalf("expected lite3 payload")
	}

	seq2, err := pool.AppendLite3(seedFrame.Payload, DurabilityFast)
	if err != nil {
		t.Fatalf("append lite3: %v", err)
	}
	frame2, err := pool.GetLite3(seq2)
	if err != nil {
		t.Fatalf("get lite3 (seq2): %v", err)
	}
	if !bytes.Equal(seedFrame.Payload, frame2.Payload) {
		t.Fatalf("lite3 payload mismatch")
	}

	timeout := uint64(50)
	stream, err := pool.OpenLite3Stream(uint64Ptr(seq2), uint64Ptr(1), &timeout)
	if err != nil {
		t.Fatalf("open lite3 stream: %v", err)
	}
	defer stream.Close()
	streamFrame, err := stream.Next()
	if err != nil {
		t.Fatalf("stream next: %v", err)
	}
	if streamFrame.Seq != seq2 {
		t.Fatalf("stream seq mismatch")
	}
	if !bytes.Equal(seedFrame.Payload, streamFrame.Payload) {
		t.Fatalf("stream payload mismatch")
	}
	if _, err := stream.Next(); !errors.Is(err, io.EOF) {
		t.Fatalf("expected eof, got %v", err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	out, errs := pool.TailLite3(ctx, TailOptions{
		SinceSeq:    uint64Ptr(seq2),
		MaxMessages: uint64Ptr(1),
		Timeout:     50 * time.Millisecond,
		Buffer:      2,
	})
	select {
	case tailFrame := <-out:
		if tailFrame == nil {
			t.Fatalf("expected frame")
		}
		if tailFrame.Seq != seq2 {
			t.Fatalf("tail seq mismatch")
		}
		if !bytes.Equal(seedFrame.Payload, tailFrame.Payload) {
			t.Fatalf("tail payload mismatch")
		}
	case err := <-errs:
		t.Fatalf("unexpected error: %v", err)
	case <-time.After(2 * time.Second):
		t.Fatalf("tail did not yield")
	}
}

func TestLite3AppendRejectsInvalidPayload(t *testing.T) {
	client := newTestClient(t)
	pool := newTestPool(t, client, "lite3-bad")

	_, err := pool.AppendLite3([]byte{0x01, 0x02, 0x03}, DurabilityFast)
	if err == nil {
		t.Fatalf("expected error")
	}
	var perr *Error
	if !errors.As(err, &perr) {
		t.Fatalf("expected plasmite error")
	}
	if perr.Kind != ErrorCorrupt && perr.Kind != ErrorUsage {
		t.Fatalf("unexpected error kind: %v", perr.Kind)
	}
}

func uint64Ptr(val uint64) *uint64 {
	return &val
}

func decodeObject(raw json.RawMessage) (map[string]any, error) {
	var out map[string]any
	if err := json.Unmarshal(raw, &out); err != nil {
		return nil, err
	}
	return out, nil
}
