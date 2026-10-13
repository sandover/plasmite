// Package udp wraps Plasmite's best-effort UDP sender and follower through the
// shared libplasmite C ABI.
package udp

/*
#cgo pkg-config: plasmite
#cgo CFLAGS: -I${SRCDIR}/../../include
#cgo LDFLAGS: -lplasmite
#include "plasmite.h"
#include <stdlib.h>
*/
import "C"

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"runtime"
	"strings"
	"sync"
	"time"
	"unsafe"

	"github.com/sandover/plasmite/bindings/go/api"
)

type Error = api.Error
type Start = api.UDPStart
type StartKind = api.UDPStartKind
type FollowOptions = api.UDPFollowOptions
type Event = api.UDPEvent
type Frame = api.UDPFrame
type Gap = api.UDPGap
type Progress = api.UDPProgress

const (
	StartNew      = api.UDPStartNew
	StartLast     = api.UDPStartLast
	StartSince    = api.UDPStartSince
	StartSequence = api.UDPStartSequence
)

func NewFollowOptions() FollowOptions { return api.NewUDPFollowOptions() }
func NewPosition() Start              { return api.UDPStartNewPosition() }
func Last(count uint64) Start         { return api.UDPStartLastCount(count) }
func Since(timestampNs uint64) Start  { return api.UDPStartSinceNs(timestampNs) }
func Sequence(seq uint64) Start       { return api.UDPStartAtSequence(seq) }

var (
	ErrClosed          = api.ErrClosed
	ErrInvalidArgument = api.ErrInvalidArgument
)

type Client struct {
	mu  sync.Mutex
	ptr *C.plsm_udp_client_t
}

type Pool struct {
	mu  sync.Mutex
	ptr *C.plsm_udp_pool_t
}

type Follow struct {
	stateMu sync.Mutex
	readMu  sync.Mutex
	ptr     *C.plsm_udp_follow_t
	closing bool
	closed  bool
}

func NewClient(endpoint string) (*Client, error) {
	if endpoint == "" || strings.ContainsRune(endpoint, '\x00') {
		return nil, invalidArgument("endpoint is required and cannot contain NUL")
	}
	cEndpoint := C.CString(endpoint)
	defer C.free(unsafe.Pointer(cEndpoint))
	var out *C.plsm_udp_client_t
	var cErr *C.plsm_error_t
	if rc := C.plsm_udp_client_new(cEndpoint, &out, &cErr); rc != 0 {
		return nil, fromCError(cErr)
	}
	return &Client{ptr: out}, nil
}

func (c *Client) OpenPool(name string) (*Pool, error) {
	if c == nil {
		return nil, closedError("UDP client")
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.ptr == nil {
		return nil, closedError("UDP client")
	}
	if name == "" || strings.ContainsRune(name, '\x00') {
		return nil, invalidArgument("pool name is required and cannot contain NUL")
	}
	cName := C.CString(name)
	defer C.free(unsafe.Pointer(cName))
	var out *C.plsm_udp_pool_t
	var cErr *C.plsm_error_t
	if rc := C.plsm_udp_client_open_pool(c.ptr, cName, &out, &cErr); rc != 0 {
		return nil, fromCError(cErr)
	}
	return &Pool{ptr: out}, nil
}

func (c *Client) Close() {
	if c == nil {
		return
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.ptr != nil {
		C.plsm_udp_client_free(c.ptr)
		c.ptr = nil
	}
}

// SendJSON reports local validation and operating-system send errors only.
// A nil error does not confirm that the server stored the message.
func (p *Pool) SendJSON(value any, tags []string) error {
	if p == nil {
		return closedError("UDP pool")
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.ptr == nil {
		return closedError("UDP pool")
	}
	for _, tag := range tags {
		if strings.ContainsRune(tag, '\x00') {
			return invalidArgument("tag cannot contain NUL")
		}
	}
	payload, err := json.Marshal(value)
	if err != nil {
		return fmt.Errorf("plasmite: encode UDP payload: %w", err)
	}
	var data *C.uint8_t
	if len(payload) > 0 {
		data = (*C.uint8_t)(unsafe.Pointer(&payload[0]))
	}
	cTags, cleanup := cStringArray(tags)
	defer cleanup()
	var cErr *C.plsm_error_t
	rc := C.plsm_udp_pool_send_json(p.ptr, data, C.size_t(len(payload)), cTags, C.size_t(len(tags)), &cErr)
	runtime.KeepAlive(payload)
	runtime.KeepAlive(tags)
	if rc != 0 {
		return fromCError(cErr)
	}
	return nil
}

func (p *Pool) SendLite3(payload []byte) error {
	if p == nil {
		return closedError("UDP pool")
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.ptr == nil {
		return closedError("UDP pool")
	}
	if len(payload) == 0 {
		return invalidArgument("Lite3 payload is required")
	}
	var cErr *C.plsm_error_t
	rc := C.plsm_udp_pool_send_lite3(
		p.ptr,
		(*C.uint8_t)(unsafe.Pointer(&payload[0])),
		C.size_t(len(payload)),
		&cErr,
	)
	runtime.KeepAlive(payload)
	if rc != 0 {
		return fromCError(cErr)
	}
	return nil
}

func (p *Pool) Follow(options FollowOptions) (*Follow, error) {
	if p == nil {
		return nil, closedError("UDP pool")
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.ptr == nil {
		return nil, closedError("UDP pool")
	}
	if options.Start.Kind > api.UDPStartSequence {
		return nil, invalidArgument("invalid UDP follow start kind")
	}
	var out *C.plsm_udp_follow_t
	var cErr *C.plsm_error_t
	rc := C.plsm_udp_pool_follow(
		p.ptr,
		C.uint32_t(options.Start.Kind),
		C.uint64_t(options.Start.Value),
		boolToUint32(options.Live),
		&out,
		&cErr,
	)
	if rc != 0 {
		return nil, fromCError(cErr)
	}
	return &Follow{ptr: out}, nil
}

func (p *Pool) Close() {
	if p == nil {
		return
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.ptr != nil {
		C.plsm_udp_pool_free(p.ptr)
		p.ptr = nil
	}
}

// Next blocks until one event arrives or ctx ends. Finite follows return
// io.EOF. Cancellation stops the current wait within 50 milliseconds.
func (f *Follow) Next(ctx context.Context) (api.UDPEvent, error) {
	if f == nil {
		return nil, closedError("UDP follow")
	}
	if ctx == nil {
		return nil, invalidArgument("context is nil")
	}
	f.readMu.Lock()
	defer f.readMu.Unlock()
	f.stateMu.Lock()
	if f.ptr == nil || f.closing || f.closed {
		f.stateMu.Unlock()
		return nil, closedError("UDP follow")
	}
	ptr := f.ptr
	f.stateMu.Unlock()

	for {
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		wait := 50 * time.Millisecond
		if deadline, ok := ctx.Deadline(); ok {
			remaining := time.Until(deadline)
			if remaining <= 0 {
				return nil, ctx.Err()
			}
			if remaining < wait {
				wait = remaining
			}
		}
		var event C.plsm_udp_event_t
		var cErr *C.plsm_error_t
		rc := C.plsm_udp_follow_next(ptr, C.uint64_t(durationMillisCeil(wait)), &event, &cErr)
		switch rc {
		case 0:
			continue
		case 1:
			return eventFromC(&event)
		case 2:
			return nil, io.EOF
		case 3:
			if err := ctx.Err(); err != nil {
				return nil, err
			}
			return nil, context.Canceled
		default:
			return nil, fromCError(cErr)
		}
	}
}

func (f *Follow) Cancel() {
	if f == nil {
		return
	}
	f.stateMu.Lock()
	defer f.stateMu.Unlock()
	if f.ptr != nil && !f.closed {
		C.plsm_udp_follow_cancel(f.ptr)
	}
}

func (f *Follow) Close() {
	if f == nil {
		return
	}
	f.stateMu.Lock()
	ptr := f.ptr
	if ptr == nil || f.closed {
		f.stateMu.Unlock()
		return
	}
	f.closing = true
	C.plsm_udp_follow_cancel(ptr)
	f.stateMu.Unlock()
	f.readMu.Lock()
	defer f.readMu.Unlock()
	f.stateMu.Lock()
	defer f.stateMu.Unlock()
	if f.ptr != nil && !f.closed {
		C.plsm_udp_follow_close(f.ptr)
		f.ptr = nil
		f.closed = true
	}
}

func eventFromC(event *C.plsm_udp_event_t) (api.UDPEvent, error) {
	defer C.plsm_udp_event_free(event)
	switch event.kind {
	case C.PLSM_UDP_EVENT_FRAME:
		payload := []byte(nil)
		if event.payload.data != nil && event.payload.len != 0 {
			payload = C.GoBytes(unsafe.Pointer(event.payload.data), C.int(event.payload.len))
		}
		var messageBuf C.plsm_buf_t
		var cErr *C.plsm_error_t
		if rc := C.plsm_udp_event_message(event, &messageBuf, &cErr); rc != 0 {
			return nil, fromCError(cErr)
		}
		message, err := api.DecodeMessage(copyAndFreeBuf(&messageBuf))
		if err != nil {
			return nil, err
		}
		return &api.UDPFrame{
			Seq:         uint64(event.seq),
			TimestampNs: uint64(event.timestamp_ns),
			Payload:     payload,
			Message:     message,
		}, nil
	case C.PLSM_UDP_EVENT_GAP:
		reasons := [...]string{"unknown", "oversized", "retention", "pressure"}
		reason := "unknown"
		if event.reason < C.uint32_t(len(reasons)) {
			reason = reasons[int(event.reason)]
		}
		return &api.UDPGap{First: uint64(event.first), Last: uint64(event.last), Reason: reason}, nil
	case C.PLSM_UDP_EVENT_PROGRESS:
		progress := &api.UDPProgress{
			StartSeq:     uint64(event.start_seq),
			NextSeq:      uint64(event.next_seq),
			Terminal:     event.terminal != 0,
			RetentionGap: event.retention_gap != 0,
		}
		if event.has_end != 0 {
			end := uint64(event.end_seq)
			progress.EndSeq = &end
		}
		return progress, nil
	default:
		return nil, fmt.Errorf("plasmite: unknown UDP event kind %d", uint32(event.kind))
	}
}

func copyAndFreeBuf(buf *C.plsm_buf_t) []byte {
	if buf == nil || buf.data == nil || buf.len == 0 {
		return nil
	}
	data := C.GoBytes(unsafe.Pointer(buf.data), C.int(buf.len))
	C.plsm_buf_free(buf)
	return data
}

func cStringArray(values []string) (**C.char, func()) {
	if len(values) == 0 {
		return nil, func() {}
	}
	cValues := make([]*C.char, len(values))
	for i, value := range values {
		cValues[i] = C.CString(value)
	}
	cleanup := func() {
		for _, value := range cValues {
			C.free(unsafe.Pointer(value))
		}
	}
	return (**C.char)(unsafe.Pointer(&cValues[0])), cleanup
}

func fromCError(cErr *C.plsm_error_t) *api.Error {
	if cErr == nil {
		return &api.Error{Kind: api.ErrorInternal, Message: "unknown error"}
	}
	defer C.plsm_error_free(cErr)
	out := &api.Error{Kind: api.ErrorKind(cErr.kind)}
	if cErr.message != nil {
		out.Message = C.GoString(cErr.message)
	}
	if cErr.path != nil {
		out.Path = C.GoString(cErr.path)
	}
	if cErr.has_seq != 0 {
		seq := uint64(cErr.seq)
		out.Seq = &seq
	}
	if cErr.has_offset != 0 {
		offset := uint64(cErr.offset)
		out.Offset = &offset
	}
	if out.Message == "" {
		out.Message = "unknown error"
	}
	return out
}

func invalidArgument(message string) error {
	return fmt.Errorf("%w: %s", api.ErrInvalidArgument, message)
}

func closedError(target string) error {
	return fmt.Errorf("%w: %s is closed", api.ErrClosed, target)
}

func boolToUint32(value bool) C.uint32_t {
	if value {
		return 1
	}
	return 0
}

func durationMillisCeil(value time.Duration) uint64 {
	if value <= 0 {
		return 0
	}
	return uint64((value + time.Millisecond - 1) / time.Millisecond)
}
