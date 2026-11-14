package local

/*
#include "borrowed.h"
*/
import "C"

import (
	"context"
	"io"
	"runtime"
	"unicode/utf8"
	"unsafe"

	"github.com/sandover/plasmite/bindings/go/internal/nativeview"
)

// ViewStream pulls one independent immutable message at a time. Close releases
// the stream; previously returned views stay in the Go heap without Close.
type ViewStream struct {
	stream *Stream
	wait   uint64
}

// OpenViewStream shares local tail ordering, filtering, matching limits and gap
// policy. Timeout retains TailOptions' bounded polling-wait meaning. The stream
// retains an independent native pool handle and can outlive its source Pool.
func (p *Pool) OpenViewStream(opts TailOptions) (*ViewStream, error) {
	if p == nil {
		return nil, closedError("pool")
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.ptr == nil {
		return nil, closedError("pool")
	}
	options := C.plsm_stream_options_t{struct_size: C.uint32_t(C.sizeof_plsm_stream_options_t)}
	if opts.SinceSeq != nil {
		options.has_since = 1
		options.since_seq = C.uint64_t(*opts.SinceSeq)
	}
	if opts.MaxMessages != nil {
		options.has_max = 1
		options.max_messages = C.uint64_t(*opts.MaxMessages)
	}
	if opts.ErrorOnGap {
		options.gap_policy = C.PLSM_GAP_ERROR
	}
	tags := make([]C.plsm_text_t, len(opts.Tags))
	var pin runtime.Pinner
	defer pin.Unpin()
	for i, tag := range opts.Tags {
		if !utf8.ValidString(tag) {
			return nil, invalidArgumentError("tag is not UTF-8")
		}
		if len(tag) > 0 {
			pointer := unsafe.StringData(tag)
			pin.Pin(pointer)
			tags[i].data = (*C.uint8_t)(unsafe.Pointer(pointer))
		}
		tags[i].len = C.size_t(len(tag))
	}
	var first *C.plsm_text_t
	if len(tags) > 0 {
		first = &tags[0]
	}
	var out *C.plsm_stream_t
	var nativeError *C.plsm_error_t
	status := C.plsm_view_stream_open(p.ptr, &options, first, C.size_t(len(tags)), &out, &nativeError)
	runtime.KeepAlive(tags)
	if status != 0 {
		return nil, fromCError(nativeError)
	}
	return &ViewStream{stream: &Stream{ptr: out}, wait: min(streamTimeoutMillis(opts.Timeout), uint64(50))}, nil
}

func (s *ViewStream) nextWait(wait uint64) (*MessageView, int, error) {
	if s == nil || s.stream == nil {
		return nil, -1, closedError("stream")
	}
	s.stream.mu.Lock()
	if s.stream.ptr == nil {
		s.stream.mu.Unlock()
		return nil, -1, closedError("stream")
	}
	result := C.plsm_go_next_view(s.stream.ptr, C.uint64_t(wait))
	s.stream.mu.Unlock()
	status := int(result.status)
	if status < 0 {
		if result.owner != nil {
			C.plsm_view_release(result.owner)
		}
		return nil, status, fromCError(result.error)
	}
	if status != 1 {
		return nil, status, nil
	}
	snapshot, err := nativeview.CopyAndRelease(unsafe.Pointer(result.owner))
	if err != nil {
		return nil, -1, err
	}
	return AdoptSnapshot(snapshot), status, nil
}

// Next blocks until a view arrives, the context ends, or this finite stream
// finishes. Context cancellation and Close interrupt idle waits within 50 ms.
func (s *ViewStream) Next(ctx context.Context) (*MessageView, error) {
	if ctx == nil {
		return nil, invalidArgumentError("context is nil")
	}
	if s == nil {
		return nil, closedError("stream")
	}
	for {
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		view, status, err := s.nextWait(s.wait)
		if err != nil {
			return nil, err
		}
		switch status {
		case 1:
			return view, nil
		case 2:
			return nil, io.EOF
		case 3:
			return nil, context.Canceled
		}
	}
}
func (s *ViewStream) Close() {
	if s != nil && s.stream != nil {
		s.stream.Close()
	}
}

// TailViews defaults to an unbuffered channel; positive Buffer opts into a queue. It
// checks context and Pool.Close between native waits and returns retained views.
func (p *Pool) TailViews(ctx context.Context, opts TailOptions) (<-chan *MessageView, <-chan error) {
	out := make(chan *MessageView, max(opts.Buffer, 0))
	errs := make(chan error, 1)
	go func() {
		defer close(out)
		defer close(errs)
		if ctx == nil {
			errs <- invalidArgumentError("context is nil")
			return
		}
		if opts.MaxMessages != nil && *opts.MaxMessages == 0 {
			return
		}
		stream, err := p.OpenViewStream(opts)
		if err != nil {
			errs <- err
			return
		}
		defer stream.Close()
		for {
			if err := ctx.Err(); err != nil {
				errs <- err
				return
			}
			if !p.tailPoolOpen() {
				errs <- closedError("pool")
				return
			}
			view, status, err := stream.nextWait(stream.wait)
			if err != nil {
				errs <- err
				return
			}
			if status == 2 || status == 3 {
				return
			}
			if status == 0 {
				continue
			}
			select {
			case out <- view:
			case <-ctx.Done():
				errs <- ctx.Err()
				return
			}
			if view.Seq() == ^uint64(0) {
				return
			}
		}
	}()
	return out, errs
}
