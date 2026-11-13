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

// AppendView encodes ordinary Go values directly and returns the committed
// immutable snapshot. Integers fit i64, finite floats keep binary64, and []byte
// becomes native bytes. Custom JSON/text hooks run once in Go's visit order.
func (p *Pool) AppendView(value any, tags []string, options ...AppendOption) (*MessageView, error) {
	if p == nil {
		return nil, closedError("pool")
	}
	input := nativeInput{}
	defer input.pin.Unpin()
	if err := input.collect(reflect.ValueOf(value), ^uintptr(0), "", false, 1); err != nil {
		return nil, err
	}
	cfg := api.ApplyAppendOptions(options...)
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

// AppendJSONView parses explicit JSON once under the strict integer-token policy.
func (p *Pool) AppendJSONView(raw []byte, tags []string, options ...AppendOption) (*MessageView, error) {
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
	cfg := api.ApplyAppendOptions(options...)
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
	return &MessageView{seq: uint64(result.seq), timestampNs: uint64(result.timestamp), encoded: encoded, dataNode: result.data, metaNode: result.meta}
}
