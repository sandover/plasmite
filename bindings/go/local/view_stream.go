package local

/*
#include "borrowed.h"
*/
import "C"

import (
	"context"
	"unsafe"

	"github.com/sandover/plasmite/bindings/go/internal/nativeview"
)

// ViewStream pulls one independent immutable message at a time. Close releases
// the stream; previously returned views stay in the Go heap without Close.
type ViewStream struct {
	stream *Stream
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
// finishes. Close interrupts an idle read. A cancellable context bounds each wait.
func (s *ViewStream) Next(ctx context.Context) (*MessageView, error) {
	if s == nil {
		return nil, closedError("stream")
	}
	return readNext(ctx, s.nextWait)
}
func (s *ViewStream) Close() {
	if s != nil && s.stream != nil {
		s.stream.Close()
	}
}
