package local

/*
#include "plasmite.h"
#include <stdlib.h>
*/
import "C"

import (
	"context"
	"encoding/json"
	"io"
	"math"
	"strings"
	"time"
	"unsafe"

	"github.com/sandover/plasmite/bindings/go/v2/api"
	"github.com/sandover/plasmite/bindings/go/v2/internal/nativeview"
)

func Create(create bool) api.OpenOption  { return api.Create(create) }
func Since(selection any) api.ReadOption { return api.Since(selection) }
func At(timeNS uint64) api.Instant       { return api.At(timeNS) }
func Last(count uint64) api.ReadOption   { return api.Last(count) }
func Tags(tags ...string) api.ReadOption { return api.Tags(tags...) }

func (p *Pool) Follow(opts ...api.ReadOption) (api.Reader, error) {
	stream, err := p.openRead(false, opts)
	if err != nil {
		return nil, err
	}
	return stream, nil
}

func (p *Pool) Fetch(opts ...api.ReadOption) (api.Reader, error) {
	stream, err := p.openRead(true, opts)
	if err != nil {
		return nil, err
	}
	return stream, nil
}

func (p *Pool) FollowView(opts ...api.ReadOption) (*ViewStream, error) {
	stream, err := p.openRead(false, opts)
	if err != nil {
		return nil, err
	}
	return &ViewStream{stream: stream}, nil
}

func (p *Pool) FetchView(opts ...api.ReadOption) (*ViewStream, error) {
	stream, err := p.openRead(true, opts)
	if err != nil {
		return nil, err
	}
	return &ViewStream{stream: stream}, nil
}

func (p *Pool) FollowLite3(opts ...api.ReadOption) (*Lite3Stream, error) {
	return p.openLite3Read(false, opts)
}

func (p *Pool) FetchLite3(opts ...api.ReadOption) (*Lite3Stream, error) {
	return p.openLite3Read(true, opts)
}

func (p *Pool) openLite3Read(finite bool, opts []api.ReadOption) (*Lite3Stream, error) {
	if p == nil {
		return nil, closedError("pool")
	}
	options, free, err := nativeReadOptions(opts)
	if err != nil {
		return nil, err
	}
	defer free()
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.ptr == nil {
		return nil, closedError("pool")
	}
	var stream *C.plsm_lite3_stream_t
	var nativeErr *C.plsm_error_t
	var status C.int
	if finite {
		status = C.plsm_pool_fetch_lite3(p.ptr, &options, &stream, &nativeErr)
	} else {
		status = C.plsm_pool_follow_lite3(p.ptr, &options, &stream, &nativeErr)
	}
	if status != 0 {
		return nil, fromCError(nativeErr)
	}
	return &Lite3Stream{ptr: stream, reading: make(chan struct{}, 1)}, nil
}

func nativeReadOptions(opts []api.ReadOption) (C.plsm_read_options_t, func(), error) {
	var cfg api.ReadConfig
	for _, opt := range opts {
		if err := opt(&cfg); err != nil {
			return C.plsm_read_options_t{}, nil, err
		}
	}
	tags, free := nativeview.TagSpans(cfg.Tags)
	return C.plsm_read_options_t{
		struct_size: C.uint32_t(C.sizeof_plsm_read_options_t),
		start_kind:  C.uint32_t(cfg.Kind),
		start_value: C.uint64_t(cfg.Value),
		tags:        (*C.plsm_text_t)(tags),
		tags_len:    C.size_t(len(cfg.Tags)),
	}, free, nil
}

func (p *Pool) openRead(finite bool, opts []api.ReadOption) (*Stream, error) {
	if p == nil {
		return nil, closedError("pool")
	}
	options, free, err := nativeReadOptions(opts)
	if err != nil {
		return nil, err
	}
	defer free()
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.ptr == nil {
		return nil, closedError("pool")
	}
	var stream *C.plsm_stream_t
	var nativeErr *C.plsm_error_t
	var status C.int
	if finite {
		status = C.plsm_pool_fetch(p.ptr, &options, &stream, &nativeErr)
	} else {
		status = C.plsm_pool_follow(p.ptr, &options, &stream, &nativeErr)
	}
	if status != 0 {
		return nil, fromCError(nativeErr)
	}
	return &Stream{ptr: stream, reading: make(chan struct{}, 1)}, nil
}

func (s *Stream) Next(ctx context.Context) (*api.Message, error) {
	if s == nil {
		return nil, closedError("stream")
	}
	return readStream(ctx, s.reading, func(wait uint64) (*api.Message, int, error) {
		raw, status, err := s.nextWait(wait)
		if err != nil || status != 1 {
			return nil, status, err
		}
		message, err := api.DecodeOwnedMessage(raw)
		return message, status, err
	})
}

func (s *Lite3Stream) Next(ctx context.Context) (*Lite3Frame, error) {
	if s == nil {
		return nil, closedError("stream")
	}
	return readStream(ctx, s.reading, s.nextWait)
}

func readStream[T any](ctx context.Context, reading chan struct{}, poll func(uint64) (*T, int, error)) (*T, error) {
	if ctx == nil {
		return nil, invalidArgumentError("context is nil")
	}
	if reading == nil {
		return nil, closedError("stream")
	}
	select {
	case reading <- struct{}{}:
		defer func() { <-reading }()
	case <-ctx.Done():
		return nil, ctx.Err()
	}
	return readNext(ctx, poll)
}

func readNext[T any](ctx context.Context, poll func(uint64) (*T, int, error)) (*T, error) {
	if ctx == nil {
		return nil, invalidArgumentError("context is nil")
	}
	for {
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		waitMS := uint64(math.MaxUint64)
		// A context can cancel one Next without closing the native reader.
		deadline, hasDeadline := ctx.Deadline()
		if hasDeadline || ctx.Done() != nil {
			wait := 50 * time.Millisecond
			if hasDeadline {
				remaining := time.Until(deadline)
				if remaining <= 0 {
					return nil, context.DeadlineExceeded
				}
				wait = min(wait, remaining)
			}
			waitMS = uint64((wait + time.Millisecond - 1) / time.Millisecond)
		}
		value, status, err := poll(waitMS)
		if err != nil {
			return nil, err
		}
		switch status {
		case 1:
			return value, nil
		case 2:
			return nil, io.EOF
		case 3:
			return nil, closedError("stream")
		}
	}
}

func (p *Pool) Info() (api.PoolInfo, error) {
	var info api.PoolInfo
	if p == nil {
		return info, closedError("pool")
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.ptr == nil {
		return info, closedError("pool")
	}
	var buf C.plsm_buf_t
	var nativeErr *C.plsm_error_t
	if C.plsm_pool_info(p.ptr, &buf, &nativeErr) != 0 {
		return info, fromCError(nativeErr)
	}
	err := json.Unmarshal(copyAndFreeBuf(&buf), &info)
	return info, err
}

func (c *Client) ListPools() ([]api.PoolInfo, error) {
	if c == nil {
		return nil, closedError("client")
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.ptr == nil {
		return nil, closedError("client")
	}
	var buf C.plsm_buf_t
	var nativeErr *C.plsm_error_t
	if C.plsm_pool_list(c.ptr, &buf, &nativeErr) != 0 {
		return nil, fromCError(nativeErr)
	}
	var pools []api.PoolInfo
	err := json.Unmarshal(copyAndFreeBuf(&buf), &pools)
	return pools, err
}

func (c *Client) DeletePool(ref PoolRef) error {
	if c == nil {
		return closedError("client")
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.ptr == nil {
		return closedError("client")
	}
	if ref == "" || strings.ContainsRune(string(ref), 0) {
		return &Error{Kind: ErrorUsage, Message: "pool reference must be nonempty and contain no NUL"}
	}
	cRef := C.CString(string(ref))
	defer C.free(unsafe.Pointer(cRef))
	var nativeErr *C.plsm_error_t
	if C.plsm_pool_delete(c.ptr, cRef, &nativeErr) != 0 {
		return fromCError(nativeErr)
	}
	return nil
}

func (p *Pool) Feed(value any, tags []string, opts ...FeedOption) (*api.Message, error) {
	if view, ok := value.(MessageView); ok {
		return p.Feed(&view, tags, opts...)
	}
	if view, ok := value.(*MessageView); ok {
		if view == nil || len(view.bytes()) == 0 {
			return nil, invalidArgumentError("missing message view")
		}
		if tags != nil {
			return nil, invalidArgumentError("cannot supply tags when forwarding a message")
		}
		return p.FeedLite3(view.bytes(), opts...)
	}
	payload, tags, err := api.EncodeFeedJSON(value, tags)
	if err != nil {
		return nil, err
	}
	raw, err := p.FeedJSON(payload, tags, opts...)
	if err != nil {
		return nil, err
	}
	return api.DecodeOwnedMessage(raw)
}

func (p *Pool) FeedReceipt(value any, tags []string, opts ...FeedOption) (*api.FeedReceipt, error) {
	if view, ok := value.(MessageView); ok {
		return p.FeedReceipt(&view, tags, opts...)
	}
	if view, ok := value.(*MessageView); ok {
		if view == nil || len(view.bytes()) == 0 {
			return nil, invalidArgumentError("missing message view")
		}
		if tags != nil {
			return nil, invalidArgumentError("cannot supply tags when forwarding a message")
		}
		return p.FeedLite3Receipt(view.bytes(), opts...)
	}
	payload, tags, err := api.EncodeFeedJSON(value, tags)
	if err != nil {
		return nil, err
	}
	return p.FeedJSONReceipt(payload, tags, opts...)
}

func (p *Pool) FeedJSONReceipt(payload []byte, tags []string, opts ...FeedOption) (*api.FeedReceipt, error) {
	return p.feedEncodedReceipt(payload, tags, api.ApplyFeedOptions(opts...).Durability, false)
}

func (p *Pool) FeedLite3Receipt(payload []byte, opts ...FeedOption) (*api.FeedReceipt, error) {
	return p.feedEncodedReceipt(payload, nil, api.ApplyFeedOptions(opts...).Durability, true)
}

func (p *Pool) feedEncodedReceipt(payload []byte, tags []string, durability Durability, lite3 bool) (*api.FeedReceipt, error) {
	if p == nil {
		return nil, closedError("pool")
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.ptr == nil {
		return nil, closedError("pool")
	}
	if len(payload) == 0 {
		return nil, invalidArgumentError("payload is required")
	}
	cTags, free := nativeview.TagSpans(tags)
	defer free()
	options := C.plsm_feed_options_t{
		struct_size: C.uint32_t(C.sizeof_plsm_feed_options_t),
		durability:  C.uint32_t(durability),
		tags:        (*C.plsm_text_t)(cTags),
		tags_len:    C.size_t(len(tags)),
	}
	var receipt C.plsm_feed_receipt_t
	var nativeErr *C.plsm_error_t
	bytes := (*C.uint8_t)(unsafe.Pointer(unsafe.SliceData(payload)))
	var status C.int
	if lite3 {
		status = C.plsm_pool_feed_lite3_receipt(p.ptr, bytes, C.size_t(len(payload)), &options, &receipt, &nativeErr)
	} else {
		status = C.plsm_pool_feed_json_receipt(p.ptr, bytes, C.size_t(len(payload)), &options, &receipt, &nativeErr)
	}
	if status != 0 {
		return nil, fromCError(nativeErr)
	}
	return &api.FeedReceipt{Seq: uint64(receipt.seq), Time: uint64(receipt.time)}, nil
}

func (p *Pool) FeedJSON(payload []byte, tags []string, opts ...FeedOption) ([]byte, error) {
	return p.feedEncoded(payload, tags, api.ApplyFeedOptions(opts...).Durability, false)
}

func (p *Pool) FeedLite3(payload []byte, opts ...FeedOption) (*api.Message, error) {
	raw, err := p.feedEncoded(payload, nil, api.ApplyFeedOptions(opts...).Durability, true)
	if err != nil {
		return nil, err
	}
	return api.DecodeOwnedMessage(raw)
}

func (p *Pool) feedEncoded(payload []byte, tags []string, durability Durability, lite3 bool) ([]byte, error) {
	if p == nil {
		return nil, closedError("pool")
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.ptr == nil {
		return nil, closedError("pool")
	}
	if len(payload) == 0 {
		return nil, invalidArgumentError("payload is required")
	}
	cTags, free := nativeview.TagSpans(tags)
	defer free()
	options := C.plsm_feed_options_t{
		struct_size: C.uint32_t(C.sizeof_plsm_feed_options_t),
		durability:  C.uint32_t(durability),
		tags:        (*C.plsm_text_t)(cTags),
		tags_len:    C.size_t(len(tags)),
	}
	var buf C.plsm_buf_t
	var nativeErr *C.plsm_error_t
	bytes := (*C.uint8_t)(unsafe.Pointer(unsafe.SliceData(payload)))
	var status C.int
	if lite3 {
		status = C.plsm_pool_feed_lite3(p.ptr, bytes, C.size_t(len(payload)), &options, &buf, &nativeErr)
	} else {
		status = C.plsm_pool_feed_json(p.ptr, bytes, C.size_t(len(payload)), &options, &buf, &nativeErr)
	}
	if status != 0 {
		return nil, fromCError(nativeErr)
	}
	return copyAndFreeBuf(&buf), nil
}
