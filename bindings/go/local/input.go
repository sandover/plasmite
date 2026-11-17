package local

/*
#include "borrowed.h"
*/
import "C"

import (
	"github.com/sandover/plasmite/bindings/go/api"
	"reflect"
	"runtime"
	"unsafe"
)

// FeedView encodes native values or forwards a message with its original tags.
func (p *Pool) FeedView(value any, tags []string, options ...FeedOption) (*MessageView, error) {
	if p == nil {
		return nil, closedError("pool")
	}
	switch message := value.(type) {
	case *MessageView:
		if message == nil || len(message.encoded) == 0 {
			return nil, invalidArgumentError("missing message view")
		}
		if tags != nil {
			return nil, invalidArgumentError("cannot supply tags when forwarding a message")
		}
		snapshot := *message // options may reassign the caller's view variable
		return p.forwardView(&snapshot, api.ApplyFeedOptions(options...).Durability)
	case *api.Message:
		if message == nil || tags != nil {
			return nil, invalidArgumentError("forwarding requires a message and no tag override")
		}
		return p.FeedJSONView(message.Data, message.Tags(), options...)
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

// FeedJSONView parses explicit JSON once under the strict integer-token policy.
func (p *Pool) FeedJSONView(raw []byte, tags []string, options ...FeedOption) (*MessageView, error) {
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
func adoptNativeInput(result C.plsm_go_input_result_t) *MessageView {
	defer C.plsm_view_release(result.owner)
	words := make([]uint32, (int(result.len)+3)/4)
	encoded := unsafe.Slice((*byte)(unsafe.Pointer(&words[0])), int(result.len))
	copy(encoded, unsafe.Slice((*byte)(unsafe.Pointer(result.bytes)), int(result.len)))
	// A checked builder already proved the immutable encoding. Copying preserves
	// its unique-key layout, offsets and proof; no second validation is needed.
	return &MessageView{seq: uint64(result.seq), timestampNs: uint64(result.timestamp), valueSnapshot: valueSnapshot{encoded: encoded}, dataNode: result.data, metaNode: result.meta}
}
