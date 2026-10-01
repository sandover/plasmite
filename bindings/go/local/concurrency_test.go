package local

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"sync"
	"testing"
	"time"

	"github.com/sandover/plasmite/bindings/go/api"
)

// Tail owns a hidden goroutine that reopens streams. Applications cannot
// serialize its pool access with foreground native calls or Close themselves.
func TestPoolTailConcurrentNativeCallsAndClose(t *testing.T) {
	client := newTestClient(t)
	pool := newTestPool(t, client, "concurrent")
	seed, err := pool.Append(map[string]any{"seed": true}, nil)
	if err != nil {
		t.Fatal(err)
	}
	frame, err := pool.GetLite3(seed.Seq)
	if err != nil {
		t.Fatal(err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	opts := TailOptions{SinceSeq: uint64Ptr(seed.Seq), Timeout: 2 * time.Millisecond, Buffer: 8}
	out, tailErrors := pool.Tail(ctx, opts)
	lite3Out, lite3Errors := pool.TailLite3(ctx, opts)
	select {
	case message := <-out:
		if message == nil || message.Seq != seed.Seq {
			t.Fatalf("JSON tail did not read seed: %#v", message)
		}
	case <-ctx.Done():
		t.Fatal("JSON tail did not start")
	}
	select {
	case received := <-lite3Out:
		if received == nil || received.Seq != seed.Seq || !bytes.Equal(received.Payload, frame.Payload) {
			t.Fatalf("Lite3 tail did not read seed: %#v", received)
		}
	case <-ctx.Done():
		t.Fatal("Lite3 tail did not start")
	}
	go func() {
		for range out {
		}
	}()
	go func() {
		for range lite3Out {
		}
	}()

	// A stream that has already opened retains its independent native handle.
	timeout := uint64(500)
	independent, err := pool.OpenStream(uint64Ptr(seed.Seq), uint64Ptr(1), &timeout)
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
					if !errors.Is(err, ErrClosed) {
						workerErrors <- err
					}
					return
				}
			}
		}()
	}
	var firstAppend sync.Once
	work(func() error {
		message, err := pool.Append(map[string]any{"foreground": true}, nil)
		if err != nil {
			return err
		}
		firstAppend.Do(func() { close(readyToClose) })
		read, err := pool.Get(message.Seq)
		if err == nil && read.Seq != message.Seq {
			return fmt.Errorf("JSON read changed sequence: %d != %d", read.Seq, message.Seq)
		}
		return err
	})
	work(func() error {
		seq, err := pool.AppendLite3(frame.Payload, DurabilityFast)
		if err != nil {
			return err
		}
		read, err := pool.GetLite3(seq)
		if err == nil && (read.Seq != seq || !bytes.Equal(read.Payload, frame.Payload)) {
			return fmt.Errorf("Lite3 read changed appended frame")
		}
		return err
	})
	work(func() error {
		stream, err := pool.OpenLite3Stream(uint64Ptr(seed.Seq), uint64Ptr(1), &timeout)
		if err != nil {
			return err
		}
		defer stream.Close()
		read, err := stream.Next()
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
	for _, errs := range []<-chan error{tailErrors, lite3Errors} {
		select {
		case err := <-errs:
			if !errors.Is(err, ErrClosed) {
				t.Fatalf("tail after pool close: expected ErrClosed, got %v", err)
			}
		case <-ctx.Done():
			t.Fatal("tail did not stop after pool close")
		}
	}
	raw, err := independent.NextJSON()
	if err != nil {
		t.Fatalf("existing stream invalidated by pool close: %v", err)
	}
	message, err := api.DecodeMessage(raw)
	if err != nil || message.Seq != seed.Seq {
		t.Fatalf("existing stream changed message: %#v, %v", message, err)
	}
	if _, err := pool.Get(seed.Seq); !errors.Is(err, ErrClosed) {
		t.Fatalf("read after close: expected ErrClosed, got %v", err)
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
					if !errors.Is(err, ErrClosed) {
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
	if _, err := client.CreatePool(PoolRefName("closed"), testPoolSizeBytes); !errors.Is(err, ErrClosed) {
		t.Fatalf("create after close: expected ErrClosed, got %v", err)
	}
}

func TestStreamsConcurrentReadAndClose(t *testing.T) {
	for _, lite3 := range []bool{false, true} {
		t.Run(fmt.Sprintf("lite3=%v", lite3), func(t *testing.T) {
			client := newTestClient(t)
			pool := newTestPool(t, client, "idle")
			timeout := uint64(30)
			var next func() error
			var closeStream func()
			if lite3 {
				stream, err := pool.OpenLite3Stream(nil, nil, &timeout)
				if err != nil {
					t.Fatal(err)
				}
				next = func() error { _, err := stream.Next(); return err }
				closeStream = stream.Close
			} else {
				stream, err := pool.OpenStream(nil, nil, &timeout)
				if err != nil {
					t.Fatal(err)
				}
				next = func() error { _, err := stream.NextJSON(); return err }
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
					if err := next(); !errors.Is(err, io.EOF) && !errors.Is(err, ErrClosed) {
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
				t.Fatal("finite-timeout stream did not finish reads and close")
			}
			close(failures)
			for err := range failures {
				t.Fatal(err)
			}
			if err := next(); !errors.Is(err, ErrClosed) {
				t.Fatalf("read after close: expected ErrClosed, got %v", err)
			}
		})
	}
}
