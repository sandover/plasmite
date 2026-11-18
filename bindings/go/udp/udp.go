// Package udp wraps Plasmite's best-effort UDP sender and follower through the
// shared libplasmite C ABI.
package udp

/*
#cgo pkg-config: plasmite
#cgo CFLAGS: -I${SRCDIR}/../../include
#cgo darwin LDFLAGS: -lplasmite.2
#cgo linux LDFLAGS: -l:libplasmite.so.2
#cgo windows LDFLAGS: -lplasmite2
#include "events.h"
#include <stdlib.h>
*/
import "C"

import (
	"context"
	"fmt"
	"io"
	"runtime"
	"strings"
	"sync"
	"time"
	"unsafe"

	"github.com/sandover/plasmite/bindings/go/api"
	"github.com/sandover/plasmite/bindings/go/internal/nativeview"
	"github.com/sandover/plasmite/bindings/go/local"
)

type Error = api.Error
type Event = api.UDPEvent
type Frame = api.UDPFrame

// ViewFrame keeps DATA in the same Go-owned view type as local reads.
type ViewFrame = api.UDPViewFrame[*local.MessageView]

// Skip reports a range the server explicitly skipped.
type Skip = api.UDPSkip
type Progress = api.UDPProgress

func Since(selection any) api.ReadOption { return api.Since(selection) }
func At(timeNS uint64) api.Instant       { return api.At(timeNS) }
func Last(count uint64) api.ReadOption   { return api.Last(count) }
func Tags(tags ...string) api.ReadOption { return api.Tags(tags...) }

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
	raw     bool
	views   bool
}

func NewClient(endpoint string) (*Client, error) {
	if major := C.plsm_abi_major(); major != 2 {
		return nil, &Error{Kind: api.ErrorUsage, Message: fmt.Sprintf("Plasmite requires ABI major 2; found %d", major)}
	}
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

func (p *Pool) FeedLite3(payload []byte, opts ...api.FeedOption) error {
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
	options := C.plsm_feed_options_t{
		struct_size: C.uint32_t(C.sizeof_plsm_feed_options_t),
		durability:  C.uint32_t(api.ApplyFeedOptions(opts...).Durability),
	}
	var cErr *C.plsm_error_t
	rc := C.plsm_udp_pool_feed_lite3(
		p.ptr,
		(*C.uint8_t)(unsafe.Pointer(&payload[0])),
		C.size_t(len(payload)),
		&options,
		&cErr,
	)
	runtime.KeepAlive(payload)
	if rc != 0 {
		return fromCError(cErr)
	}
	return nil
}

func (p *Pool) Feed(value any, tags []string, opts ...api.FeedOption) error {
	if view, ok := value.(local.MessageView); ok {
		return p.Feed(&view, tags, opts...)
	}
	if view, ok := value.(*local.MessageView); ok {
		if view == nil {
			return invalidArgument("missing message view")
		}
		if tags != nil {
			return invalidArgument("cannot supply tags when forwarding a message")
		}
		return p.FeedLite3(view.Lite3(), opts...)
	}
	payload, tags, err := api.EncodeFeedJSON(value, tags)
	if err != nil {
		return err
	}
	if p == nil {
		return closedError("UDP pool")
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.ptr == nil {
		return closedError("UDP pool")
	}
	cTags, free := nativeview.TagSpans(tags)
	defer free()
	options := C.plsm_feed_options_t{
		struct_size: C.uint32_t(C.sizeof_plsm_feed_options_t),
		durability:  C.uint32_t(api.ApplyFeedOptions(opts...).Durability),
		tags:        (*C.plsm_text_t)(cTags),
		tags_len:    C.size_t(len(tags)),
	}
	var nativeErr *C.plsm_error_t
	if C.plsm_udp_pool_feed_json(p.ptr, (*C.uint8_t)(unsafe.Pointer(unsafe.SliceData(payload))),
		C.size_t(len(payload)), &options, &nativeErr) != 0 {
		return fromCError(nativeErr)
	}
	return nil
}

func (p *Pool) Follow(opts ...api.ReadOption) (*Follow, error) {
	return p.read(true, false, false, opts)
}
func (p *Pool) Fetch(opts ...api.ReadOption) (*Follow, error) {
	return p.read(false, false, false, opts)
}
func (p *Pool) FollowView(opts ...api.ReadOption) (*Follow, error) {
	return p.read(true, true, false, opts)
}
func (p *Pool) FetchView(opts ...api.ReadOption) (*Follow, error) {
	return p.read(false, true, false, opts)
}
func (p *Pool) FollowLite3(opts ...api.ReadOption) (*Follow, error) {
	return p.read(true, false, true, opts)
}
func (p *Pool) FetchLite3(opts ...api.ReadOption) (*Follow, error) {
	return p.read(false, false, true, opts)
}

func (p *Pool) read(live, views, raw bool, opts []api.ReadOption) (*Follow, error) {
	if p == nil {
		return nil, closedError("UDP pool")
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.ptr == nil {
		return nil, closedError("UDP pool")
	}
	var cfg api.ReadConfig
	for _, opt := range opts {
		if err := opt(&cfg); err != nil {
			return nil, err
		}
	}
	tags, free := nativeview.TagSpans(cfg.Tags)
	defer free()
	options := C.plsm_read_options_t{
		struct_size: C.uint32_t(C.sizeof_plsm_read_options_t),
		start_kind:  C.uint32_t(cfg.Kind),
		start_value: C.uint64_t(cfg.Value),
		tags:        (*C.plsm_text_t)(tags),
		tags_len:    C.size_t(len(cfg.Tags)),
	}
	var out *C.plsm_udp_follow_t
	var nativeErr *C.plsm_error_t
	if C.plsm_udp_pool_read(p.ptr, &options, boolToUint32(live), boolToUint32(views), &out, &nativeErr) != 0 {
		return nil, fromCError(nativeErr)
	}
	return &Follow{ptr: out, raw: raw, views: views}, nil
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
	if f.ptr == nil || f.closing {
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
				// Deadline time can pass before the context timer publishes Err.
				if err := ctx.Err(); err != nil {
					return nil, err
				}
				return nil, context.DeadlineExceeded
			}
			if remaining < wait {
				wait = remaining
			}
		}
		result := C.plsm_go_udp_next(ptr, C.uint64_t(durationMillisCeil(wait)), boolToUint32(f.views))
		event := result.event
		owner := result.owner
		rc := result.status
		cErr := result.error

		switch rc {
		case 0:
			continue
		case 1:
			if f.views && event.kind == C.PLSM_UDP_EVENT_FRAME {
				snapshot, err := nativeview.CopyAndRelease(unsafe.Pointer(owner))
				C.plsm_go_udp_event_destroy(event)
				if err != nil {
					return nil, err
				}
				view := local.AdoptSnapshot(snapshot)
				return &ViewFrame{Seq: view.Seq(), Time: view.Time(), View: view}, nil
			}
			return eventFromC(event, f.raw)
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
	if f.ptr != nil {
		C.plsm_udp_follow_cancel(f.ptr)
	}
}

func (f *Follow) Close() {
	if f == nil {
		return
	}
	f.stateMu.Lock()
	ptr := f.ptr
	if ptr == nil {
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
	if f.ptr != nil {
		C.plsm_udp_follow_close(f.ptr)
		f.ptr = nil
	}
}

func eventFromC(event C.plsm_udp_event_t, raw bool) (api.UDPEvent, error) {
	defer C.plsm_go_udp_event_destroy(event)
	switch event.kind {
	case C.PLSM_UDP_EVENT_FRAME:
		payload := []byte(nil)
		if event.payload.data != nil && event.payload.len != 0 {
			payload = C.GoBytes(unsafe.Pointer(event.payload.data), C.int(event.payload.len))
		}
		frame := &api.UDPFrame{Seq: uint64(event.seq), Time: uint64(event.time), Payload: payload}
		if raw {
			return frame, nil
		}
		result := C.plsm_go_udp_message(event)
		if result.status != 0 {
			return nil, fromCError(result.error)
		}
		message, err := api.DecodeOwnedMessage(copyAndFreeBuf(result.buffer))
		if err != nil {
			return nil, err
		}
		frame.Message = message
		return frame, nil
	case C.PLSM_UDP_EVENT_SKIP:
		reasons := [...]string{"unknown", "oversized", "retention", "pressure"}
		reason := "unknown"
		if event.reason < C.uint32_t(len(reasons)) {
			reason = reasons[int(event.reason)]
		}
		return &api.UDPSkip{First: uint64(event.first), Last: uint64(event.last), Reason: reason}, nil
	case C.PLSM_UDP_EVENT_PROGRESS:
		progress := &api.UDPProgress{
			StartSeq: uint64(event.start_seq),
			NextSeq:  uint64(event.next_seq),
			Terminal: event.terminal != 0,
		}
		if event.has_end != 0 {
			end := uint64(event.end_seq)
			progress.EndSeq = &end
		}
		return progress, nil
	default:
		return nil, &Error{Kind: api.ErrorInternal, Message: fmt.Sprintf("unknown UDP event kind %d", uint32(event.kind))}
	}
}

func copyAndFreeBuf(buf C.plsm_buf_t) []byte {
	if buf.data == nil || buf.len == 0 {
		return nil
	}
	data := C.GoBytes(unsafe.Pointer(buf.data), C.int(buf.len))
	C.plsm_go_udp_buffer_destroy(buf)
	return data
}

func fromCError(cErr *C.plsm_error_t) *api.Error {
	if cErr == nil {
		return &api.Error{Kind: api.ErrorInternal, Message: "unknown error"}
	}
	defer C.plsm_error_free(cErr)
	out := &api.Error{Kind: api.ErrorKind(cErr.kind), Missing: uint64(cErr.missing), NextSeq: uint64(cErr.next_seq)}
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
	return api.InvalidArgumentError(message)
}

func closedError(target string) error {
	return api.ClosedError(target)
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
