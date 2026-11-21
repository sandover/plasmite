package local

/*
#include "borrowed.h"
*/
import "C"

import (
	"context"
	"unsafe"

	"github.com/sandover/plasmite/bindings/go/v2/api"
	"github.com/sandover/plasmite/bindings/go/v2/internal/nativeview"
)

// viewStream pulls one independent immutable message at a time. Close releases
// the stream; previously returned views stay in the Go heap without Close.
type viewStream struct {
	stream *stream
}

func (s *viewStream) nextWait(wait uint64) (*messageView, int, error) {
	if s == nil || s.stream == nil {
		return nil, -1, closedError("stream")
	}
	return s.stream.nextView(wait)
}

func (s *stream) nextView(wait uint64) (*messageView, int, error) {
	s.mu.Lock()
	if s.ptr == nil {
		s.mu.Unlock()
		return nil, -1, closedError("stream")
	}
	result := C.plsm_go_next_view(s.ptr, C.uint64_t(wait))
	s.mu.Unlock()
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
	return adoptSnapshot(snapshot), status, nil
}

func (s *stream) nextMessage(wait uint64) (*api.Message, int, error) {
	view, status, err := s.nextView(wait)
	if view == nil {
		return nil, status, err
	}
	return view.message(), status, nil
}

// Next blocks until a view arrives, the context ends, or this finite stream
// finishes. Close interrupts an idle read. A cancellable context bounds each wait.
func (s *viewStream) Next(ctx context.Context) (*messageView, error) {
	if s == nil || s.stream == nil {
		return nil, closedError("stream")
	}
	return readStream(ctx, s.stream.reading, s.nextWait)
}
func (s *viewStream) Close() {
	if s != nil && s.stream != nil {
		s.stream.Close()
	}
}
