package local

import (
	"bytes"
	"context"
	"errors"
	"github.com/sandover/plasmite/bindings/go/api"
	"io"
	"math"
	"testing"
	"time"
)

func TestViewStreamsFilterMatchingLimitsAndKeepOwners(t *testing.T) {
	pool := newTestPool(t, newTestClient(t), "stream-views").(*Pool)
	for _, tag := range []string{"skip", "α\x00β", "skip", "α\x00β"} {
		if _, err := pool.AppendView(map[string]any{"exact": int64(math.MaxInt64), "negative": math.Copysign(0, -1), "blob": []byte{0, 255}}, []string{tag}); err != nil {
			t.Fatal(err)
		}
	}
	stream, err := pool.OpenViewFollow(FollowOptions{Start: Sequence(1), MaxMessages: uint64Ptr(2), Tags: []string{"α\x00β"}})
	if err != nil {
		t.Fatal(err)
	}
	var retained []*MessageView
	for {
		view, err := stream.Next(context.Background())
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			t.Fatal(err)
		}
		retained = append(retained, view)
	}
	stream.Close()
	pool.Close()
	if len(retained) != 2 || retained[0].Seq() != 2 || retained[1].Seq() != 4 {
		t.Fatalf("wrong accepted identities: %v", retained)
	}
	var out struct {
		Exact    int64
		Negative float64
		Blob     []byte
	}
	if err = retained[0].Decode(&out); err != nil {
		t.Fatal(err)
	}
	if out.Exact != math.MaxInt64 || !math.Signbit(out.Negative) || !bytes.Equal(out.Blob, []byte{0, 255}) {
		t.Fatalf("lost native data: %+v", out)
	}
}

func TestViewStreamsTransferDuplicateProofAndDetachedBytes(t *testing.T) {
	pool := newTestPool(t, newTestClient(t), "duplicate-stream").(*Pool)
	original, err := pool.AppendView(map[string]any{"ab": 1, "bA": 2}, nil)
	if err != nil {
		t.Fatal(err)
	}
	encoded := original.Lite3()
	key := bytes.Index(encoded, []byte("bA\x00"))
	if key < 0 {
		t.Fatal("missing fixture key")
	}
	copy(encoded[key:key+2], "ab")
	seq, err := pool.AppendLite3(encoded, DurabilityFast)
	if err != nil {
		t.Fatal(err)
	}
	stream, err := pool.OpenViewFollow(FollowOptions{Start: Sequence(seq), MaxMessages: uint64Ptr(1)})
	if err != nil {
		t.Fatal(err)
	}
	defer stream.Close()
	pool.Close() // the opened low-level stream owns an independent native handle
	view, err := stream.Next(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	var out struct {
		AB int `json:"ab"`
	}
	if err = view.Decode(&out); err != nil {
		t.Fatal(err)
	}
	if out.AB != 2 {
		t.Fatalf("lost canonical duplicate proof: %d", out.AB)
	}
	detached := view.Lite3()
	clear(detached)
	data, err := view.DataJSON()
	if err != nil || string(data) != `{"ab":2}` {
		t.Fatalf("mutation escaped: %s %v", data, err)
	}
}

func TestViewStreamsCancellationCloseAndGapPolicy(t *testing.T) {
	pool := newTestPool(t, newTestClient(t), "idle-views").(*Pool)
	stream, err := pool.OpenViewFollow(FollowOptions{Start: All()})
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Millisecond)
	defer cancel()
	start := time.Now()
	if _, err = stream.Next(ctx); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("wrong cancel: %v", err)
	}
	if time.Since(start) > time.Second {
		t.Fatal("cancel blocked")
	}
	done := make(chan error, 1)
	go func() { _, err := stream.Next(context.Background()); done <- err }()
	time.Sleep(time.Millisecond)
	stream.Close()
	select {
	case err = <-done:
		if !errors.Is(err, ErrClosed) {
			t.Fatalf("wrong close: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("close blocked")
	}
	ctx2, cancel2 := context.WithCancel(context.Background())
	out, errs := pool.FollowViews(ctx2, FollowOptions{Start: All(), Buffer: 0})
	cancel2()
	for range out {
	}
	if err = <-errs; !errors.Is(err, context.Canceled) {
		t.Fatalf("wrong channel cancel: %v", err)
	}
	for i := 0; i < 800; i++ {
		if _, err := pool.AppendView(map[string]any{"padding": string(bytes.Repeat([]byte{'x'}, 2048))}, nil); err != nil {
			t.Fatal(err)
		}
	}
	strict, err := pool.OpenViewFollow(FollowOptions{Start: Sequence(1), ErrorOnGap: true})
	if err != nil {
		t.Fatal(err)
	}
	defer strict.Close()
	if _, err = strict.Next(context.Background()); err == nil {
		t.Fatal("missing retention gap")
	} else {
		var native *Error
		if !errors.As(err, &native) || native.Kind != ErrorRetentionGap {
			t.Fatalf("wrong gap: %v", err)
		}
	}
}

func TestWholeViewForwardingSharesProofAndRetagsExplicitly(t *testing.T) {
	client := newTestClient(t)
	sourcePool := newTestPool(t, client, "forward-source").(*Pool)
	destination := newTestPool(t, client, "forward-target").(*Pool)
	source, err := sourcePool.AppendView(map[string]any{"value": int64(math.MaxInt64), "blob": []byte{0, 255}}, []string{"keep"})
	if err != nil {
		t.Fatal(err)
	}
	if _, err = destination.AppendView(map[string]any{}, nil); err != nil {
		t.Fatal(err)
	}
	expected := source.Lite3()
	forwarded, err := destination.AppendView(source, nil)
	if err != nil {
		t.Fatal(err)
	}
	if forwarded.Seq() != 2 || !bytes.Equal(forwarded.Lite3(), expected) || &forwarded.encoded[0] != &source.encoded[0] {
		t.Fatal("whole forwarding copied/changed the sealed payload or identity")
	}
	cleared, err := destination.AppendView(source, []string{})
	if err != nil {
		t.Fatal(err)
	}
	var metadata struct{ Tags []string }
	if err = cleared.Meta().Decode(&metadata); err != nil {
		t.Fatal(err)
	}
	if len(metadata.Tags) != 0 {
		t.Fatalf("explicit empty tags retained %v", metadata.Tags)
	}
	// Apply options after the source snapshot, outside the destination mutex.
	moved, err := destination.AppendView(source, nil, func(config *api.AppendConfig) { *source = MessageView{} })
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(moved.Lite3(), expected) {
		t.Fatal("option reassignment changed the earlier source")
	}
	closing := newTestPool(t, client, "forward-close").(*Pool)
	if _, err = closing.AppendView(forwarded, nil, func(*api.AppendConfig) { closing.Close() }); !errors.Is(err, ErrClosed) {
		t.Fatalf("reentrant forward close: %v", err)
	}
	sourcePool.Close()
	destination.Close()
	var decoded struct {
		Value int64
		Blob  []byte
	}
	if err = forwarded.Decode(&decoded); err != nil || decoded.Value != math.MaxInt64 || !bytes.Equal(decoded.Blob, []byte{0, 255}) {
		t.Fatalf("retained forward: %+v %v", decoded, err)
	}
}
