package local

/*
#include "borrowed.h"
*/
import "C"

import (
	"github.com/sandover/plasmite/bindings/go/v2/api"
	"github.com/sandover/plasmite/bindings/go/v2/internal/message"
	"github.com/sandover/plasmite/bindings/go/v2/internal/nativeview"
	"reflect"
	"runtime"
	"unsafe"
)

// feedView encodes native values or forwards a message with its original tags.
func (p *Pool) feedView(value any, tags []string, options ...FeedOption) (*messageView, error) {
	if p == nil {
		return nil, closedError("pool")
	}
	switch message := value.(type) {
	case messageView:
		return p.feedView(&message, tags, options...)
	case *messageView:
		if message == nil || len(message.encoded) == 0 {
			return nil, invalidArgumentError("missing message view")
		}
		if tags != nil {
			return nil, invalidArgumentError("cannot supply tags when forwarding a message")
		}
		snapshot := *message // options may reassign the caller's view variable
		return p.forwardView(&snapshot, api.ApplyFeedOptions(options...).Durability)
	}
	input := nativeInput{}
	defer input.pin.Unpin()
	if err := input.collect(reflect.ValueOf(value), ^uintptr(0), "", false, 1); err != nil {
		return nil, err
	}
	cfg := api.ApplyFeedOptions(options...)
	if err := input.tagValues(tags); err != nil {
		return nil, err
	}
	result, err := p.appendNative(&input, cfg.Durability)
	runtime.KeepAlive(value)
	if err != nil {
		return nil, err
	}
	return adoptNativeInput(result), nil
}

func (p *Pool) feedLite3View(raw []byte, tags []string, options ...FeedOption) (*messageView, error) {
	if tags != nil {
		return nil, invalidArgumentError("cannot supply tags when forwarding a message")
	}
	// The caller may supply a borrowed immutable snapshot or caller-owned bytes.
	return p.feedEncodedView(C.PLSM_FEED_LITE3, raw, nil, options...)
}

func (p *Pool) feedMessage(input *api.Message, tags []string, options ...FeedOption) (*messageView, error) {
	if p == nil {
		return nil, closedError("pool")
	}
	source, ok := message.StoredEncoding(input).(*messageView)
	if !ok {
		return nil, invalidArgumentError("missing message")
	}
	if tags != nil {
		return nil, invalidArgumentError("cannot supply tags when forwarding a message")
	}
	durability := api.ApplyFeedOptions(options...).Durability
	p.mu.Lock()
	if p.ptr == nil {
		p.mu.Unlock()
		return nil, closedError("pool")
	}
	result := C.plsm_go_forward_checked_view(p.ptr, source.pointer(), C.size_t(len(source.encoded)), source.metaNode, C.uint32_t(durability))
	p.mu.Unlock()
	runtime.KeepAlive(source)
	switch result.status {
	case 1:
		return p.feedEncodedWithDurability(C.PLSM_FEED_LITE3, source.bytes(), nil, durability)
	case 0:
		forwarded := *source
		forwarded.seq, forwarded.timeNS = uint64(result.seq), uint64(result.timestamp)
		return &forwarded, nil
	default:
		return nil, fromCError(result.error)
	}
}

func (p *Pool) feedEncodedView(kind C.uint32_t, raw []byte, tags []string, opts ...FeedOption) (*messageView, error) {
	if p == nil {
		return nil, closedError("pool")
	}
	if len(raw) == 0 {
		return nil, invalidArgumentError("payload is required")
	}
	// A user-provided option may modify or replace caller-owned bytes.
	if len(opts) > 0 {
		raw = append([]byte(nil), raw...)
	}
	return p.feedEncodedWithDurability(kind, raw, tags, api.ApplyFeedOptions(opts...).Durability)
}

func (p *Pool) feedEncodedWithDurability(kind C.uint32_t, raw []byte, tags []string, durability Durability) (*messageView, error) {
	cTags, free := nativeview.TagSpans(tags)
	defer free()
	options := C.plsm_feed_options_t{
		struct_size: C.uint32_t(C.sizeof_plsm_feed_options_t),
		durability:  C.uint32_t(durability),
		tags:        (*C.plsm_text_t)(cTags),
		tags_len:    C.size_t(len(tags)),
	}
	var owner *C.plsm_message_t
	var nativeErr *C.plsm_error_t
	p.mu.Lock()
	if p.ptr == nil {
		p.mu.Unlock()
		return nil, closedError("pool")
	}
	status := C.plsm_pool_feed(p.ptr, kind, unsafe.Pointer(unsafe.SliceData(raw)), C.size_t(len(raw)), &options, &owner, &nativeErr)
	p.mu.Unlock()
	runtime.KeepAlive(raw)
	if status != 0 {
		return nil, fromCError(nativeErr)
	}
	snapshot, err := nativeview.CopyAndRelease(unsafe.Pointer(owner))
	if err != nil {
		return nil, err
	}
	return adoptSnapshot(snapshot), nil
}

// feedJSONView parses explicit JSON once under the strict integer-token policy.
func (p *Pool) feedJSONView(raw []byte, tags []string, options ...FeedOption) (*messageView, error) {
	if p == nil {
		return nil, closedError("pool")
	}
	input := nativeInput{}
	defer input.pin.Unpin()
	if len(options) > 0 {
		raw = append([]byte(nil), raw...)
	} // options may mutate caller input
	index := input.add(^uintptr(0), "", false, 1, inputJSON)
	input.nodes[index].text = input.bytes(raw)
	cfg := api.ApplyFeedOptions(options...)
	if err := input.tagValues(tags); err != nil {
		return nil, err
	}
	result, err := p.appendNative(&input, cfg.Durability)
	runtime.KeepAlive(raw)
	if err != nil {
		return nil, err
	}
	return adoptNativeInput(result), nil
}
func (p *Pool) appendNative(input *nativeInput, durability Durability) (C.plsm_go_input_result_t, error) {
	p.mu.Lock()
	if p.ptr == nil {
		p.mu.Unlock()
		return C.plsm_go_input_result_t{}, closedError("pool")
	}
	var tags *C.plsm_go_input_span_t
	if len(input.tags) > 0 {
		tags = &input.tags[0]
	}
	result := C.plsm_go_append_input(p.ptr, &input.nodes[0], C.size_t(len(input.nodes)), tags, C.size_t(len(input.tags)), C.uint32_t(durability))
	p.mu.Unlock() // result owns its bytes independently of the pool
	runtime.KeepAlive(input)
	if result.status != 0 {
		return result, fromCError(result.error)
	}
	return result, nil
}
func adoptNativeInput(result C.plsm_go_input_result_t) *messageView {
	defer C.plsm_view_release(result.owner)
	words := make([]uint32, (int(result.len)+3)/4)
	encoded := unsafe.Slice((*byte)(unsafe.Pointer(&words[0])), int(result.len))
	copy(encoded, unsafe.Slice((*byte)(unsafe.Pointer(result.bytes)), int(result.len)))
	// A checked builder already proved the immutable encoding. Copying preserves
	// its unique-key layout, offsets and proof; no second validation is needed.
	return &messageView{seq: uint64(result.seq), timeNS: uint64(result.timestamp), valueSnapshot: valueSnapshot{encoded: encoded}, dataNode: result.data, metaNode: result.meta}
}
