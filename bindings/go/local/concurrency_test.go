package local

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"math"
	"sync"
	"testing"
	"time"

	"github.com/sandover/plasmite/bindings/go/api"
)

func TestPoolConcurrentNativeCallsAndClose(t *testing.T) {
	client := newTestClient(t)
	pool := newTestPool(t, client, "concurrent")
	seed, err := pool.Feed(map[string]any{"seed": true}, nil)
	if err != nil {
		t.Fatal(err)
	}
	frame, err := pool.getLite3(seed.Seq)
	if err != nil {
		t.Fatal(err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	// A reader that has already opened retains its independent native handle.
	independent, err := pool.Fetch(Since(seed.Seq))
	if err != nil {
		t.Fatal(err)
	}
	defer independent.Close()

	var workers sync.WaitGroup
	workerErrors := make(chan error, 3)
	readyToClose := make(chan struct{})
	start := make(chan struct{})
	work := func(operation func() error) {
		workers.Add(1)
		go func() {
			defer workers.Done()
			<-start
			for i := 0; i < 1000; i++ {
				if err := operation(); err != nil {
					if !isUsage(err) {
						workerErrors <- err
					}
					return
				}
			}
		}()
	}
	var firstAppend sync.Once
	work(func() error {
		message, err := pool.Feed(map[string]any{"foreground": true}, nil)
		if err != nil {
			return err
		}
		firstAppend.Do(func() { close(readyToClose) })
		read, err := getTestMessage(pool, message.Seq)
		if err == nil && read.Seq != message.Seq {
			return fmt.Errorf("JSON read changed sequence: %d != %d", read.Seq, message.Seq)
		}
		return err
	})
	work(func() error {
		seq, err := feedLite3Sequence(pool, frame.Payload, DurabilityFast)
		if err != nil {
			return err
		}
		read, err := pool.getLite3(seq)
		if err == nil && (read.Seq != seq || !bytes.Equal(read.Payload, frame.Payload)) {
			return fmt.Errorf("Lite3 read changed appended frame")
		}
		return err
	})
	work(func() error {
		stream, err := pool.FetchLite3(Since(seed.Seq))
		if err != nil {
			return err
		}
		defer stream.Close()
		read, err := stream.Next(ctx)
		if err == nil && read.Seq != seed.Seq {
			return fmt.Errorf("independent Lite3 stream changed sequence")
		}
		return err
	})
	close(start)
	select {
	case <-readyToClose:
	case <-ctx.Done():
		t.Fatal("foreground append did not start")
	}
	pool.Close()
	pool.Close()
	completed := make(chan struct{})
	go func() { workers.Wait(); close(completed) }()
	select {
	case <-completed:
	case <-ctx.Done():
		t.Fatal("foreground operations did not stop after close")
	}
	close(workerErrors)
	for err := range workerErrors {
		t.Fatalf("foreground native operation: %v", err)
	}
	message, err := independent.Next(ctx)
	if err != nil || message.Seq != seed.Seq {
		t.Fatalf("existing stream after pool close: %#v, %v", message, err)
	}

	if _, err := getTestMessage(pool, seed.Seq); !isUsage(err) {
		t.Fatalf("read after close: expected ErrClosed, got %v", err)
	}
}

func TestNativeWaitExpiryKeepsTheCursor(t *testing.T) {
	client := newTestClient(t)
	pool := newTestPool(t, client, "wait-position")
	stream, err := pool.openRead(false, []api.ReadOption{Since(uint64(1))})
	if err != nil {
		t.Fatal(err)
	}
	defer stream.Close()
	if _, status, err := stream.nextWait(0); err != nil || status != 0 {
		t.Fatalf("initial poll: %d %v", status, err)
	}
	seed, err := pool.Feed(map[string]any{"waited": true}, nil)
	if err != nil {
		t.Fatal(err)
	}
	raw, status, err := stream.nextWait(0)
	if err != nil || status != 1 {
		t.Fatalf("next poll: %d %v", status, err)
	}
	message, err := api.DecodeMessage(raw)
	if err != nil || message.Seq != seed.Seq {
		t.Fatalf("checkpoint changed: %v %v", message, err)
	}
	if _, status, err := stream.nextWait(0); err != nil || status != 0 {
		t.Fatalf("quiet follow: %d %v", status, err)
	}
}

func TestClientConcurrentOpenAndClose(t *testing.T) {
	client := newTestClient(t)
	newTestPool(t, client, "existing")
	start := make(chan struct{})
	opened := make(chan struct{})
	var firstOpen sync.Once
	var workers sync.WaitGroup
	failures := make(chan error, 4)
	for worker := 0; worker < 4; worker++ {
		workers.Add(1)
		go func() {
			defer workers.Done()
			<-start
			for i := 0; i < 1000; i++ {
				pool, err := client.OpenPool(PoolRefName("existing"))
				if err != nil {
					if !isUsage(err) {
						failures <- err
					}
					return
				}
				firstOpen.Do(func() { close(opened) })
				pool.Close()
			}
		}()
	}
	close(start)
	select {
	case <-opened:
	case <-time.After(2 * time.Second):
		t.Fatal("client opens did not start")
	}
	client.Close()
	completed := make(chan struct{})
	go func() { workers.Wait(); close(completed) }()
	select {
	case <-completed:
	case <-time.After(2 * time.Second):
		t.Fatal("client operations did not stop after close")
	}
	close(failures)
	for err := range failures {
		t.Fatalf("concurrent client operation: %v", err)
	}
	if _, err := client.CreatePool(PoolRefName("closed"), testPoolSizeBytes); !isUsage(err) {
		t.Fatalf("create after close: expected ErrClosed, got %v", err)
	}
}

func TestReadNextWithBackgroundWaitsWithoutDeadline(t *testing.T) {
	_, err := readNext(context.Background(), func(wait uint64) (*int, int, error) {
		if wait != math.MaxUint64 {
			t.Fatalf("idle read asked to wake after %d ms", wait)
		}
		return nil, 2, nil
	})
	if err != io.EOF {
		t.Fatalf("read completion: got %v, want EOF", err)
	}
}

func TestStreamsConcurrentReadAndClose(t *testing.T) {
	for _, lite3 := range []bool{false, true} {
		t.Run(fmt.Sprintf("lite3=%v", lite3), func(t *testing.T) {
			client := newTestClient(t)
			pool := newTestPool(t, client, "idle")
			ctx := context.Background()
			var next func() error
			var closeStream func()
			if lite3 {
				stream, err := pool.FollowLite3()
				if err != nil {
					t.Fatal(err)
				}
				next = func() error { _, err := stream.Next(ctx); return err }
				closeStream = stream.Close
			} else {
				stream, err := pool.Follow()
				if err != nil {
					t.Fatal(err)
				}
				next = func() error { _, err := stream.Next(ctx); return err }
				closeStream = stream.Close
			}
			defer closeStream()
			start := make(chan struct{})
			var workers sync.WaitGroup
			failures := make(chan error, 4)
			for reader := 0; reader < 4; reader++ {
				workers.Add(1)
				go func() {
					defer workers.Done()
					<-start
					if err := next(); !errors.Is(err, io.EOF) && !isUsage(err) {
						failures <- fmt.Errorf("idle read: expected EOF or ErrClosed, got %v", err)
					}
				}()
			}
			for closer := 0; closer < 2; closer++ {
				workers.Add(1)
				go func() { defer workers.Done(); <-start; closeStream() }()
			}
			close(start)
			completed := make(chan struct{})
			go func() { workers.Wait(); close(completed) }()
			select {
			case <-completed:
			case <-time.After(2 * time.Second):
				t.Fatal("idle stream did not finish reads and close")
			}
			close(failures)
			for err := range failures {
				t.Fatal(err)
			}
			if err := next(); !isUsage(err) {
				t.Fatalf("read after close: expected ErrClosed, got %v", err)
			}
		})
	}
}

func TestConcurrentReadObservesDeadlineWhileQueued(t *testing.T) {
	pool := newTestPool(t, newTestClient(t), "busy-readers")
	json, err := pool.Follow()
	if err != nil {
		t.Fatal(err)
	}
	defer json.Close()
	lite3, err := pool.FollowLite3()
	if err != nil {
		t.Fatal(err)
	}
	defer lite3.Close()
	view, err := pool.FollowView()
	if err != nil {
		t.Fatal(err)
	}
	defer view.Close()
	for _, tc := range []struct {
		name    string
		reading chan struct{}
		read    func(context.Context) error
	}{
		{"json", json.(*Stream).reading, func(ctx context.Context) error { _, err := json.Next(ctx); return err }},
		{"lite3", lite3.reading, func(ctx context.Context) error { _, err := lite3.Next(ctx); return err }},
		{"view", view.stream.reading, func(ctx context.Context) error { _, err := view.Next(ctx); return err }},
	} {
		t.Run(tc.name, func(t *testing.T) {
			tc.reading <- struct{}{}
			defer func() { <-tc.reading }()
			ctx, cancel := context.WithTimeout(context.Background(), 10*time.Millisecond)
			defer cancel()
			result := make(chan error, 1)
			go func() { result <- tc.read(ctx) }()
			select {
			case err := <-result:
				if !errors.Is(err, context.DeadlineExceeded) {
					t.Fatalf("overlapping read: %v", err)
				}
			case <-time.After(time.Second):
				t.Fatal("overlapping read waited for the active read")
			}
		})
	}
}

func TestHandleOptionsCanCallBackIntoHandle(t *testing.T) {
	client, err := NewClient(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	pool, err := client.CreatePool("option-callback", testPoolSizeBytes)
	if err != nil {
		client.Close()
		t.Fatal(err)
	}
	result := make(chan error, 1)
	go func() {
		other, err := client.OpenPool("option-callback", func(*api.OpenConfig) {
			_, err := client.ListPools()
			if err != nil {
				t.Error(err)
			}
		})
		if err != nil {
			result <- err
			return
		}
		other.Close()
		reader, err := pool.Fetch(func(*api.ReadConfig) error {
			_, err := pool.Info()
			return err
		})
		if err == nil {
			reader.Close()
		}
		result <- err
	}()
	select {
	case err := <-result:
		pool.Close()
		client.Close()
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(time.Second):
		t.Fatal("option callback could not use the same handle")
	}
}
