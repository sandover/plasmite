package local

/*
#include "borrowed.h"
*/
import "C"

import (
	"encoding/binary"
	"encoding/json"
	"runtime"
	"strings"
	"sync"
	"unsafe"
)

// MessageView owns an immutable Lite3 snapshot in the Go heap. Children retain
// the original buffers even when callers reassign this message wrapper. Closing the pool or replacing its records does not expire it.
// JSON, Decode and scalar/string extraction create independent requested values.
// Ordinary messages need no Close.
type MessageView struct {
	seq         uint64
	timestampNs uint64
	valueSnapshot
	dataNode C.size_t
	metaNode C.size_t
}

// valueSnapshot copies immutable slice headers, not their bytes. Children bind
// offsets to these buffers rather than to a caller-reassignable message wrapper.
type valueSnapshot struct {
	encoded    []byte
	duplicates []C.size_t
}

func (v valueSnapshot) pointer() *C.uint8_t { return (*C.uint8_t)(unsafe.Pointer(&v.encoded[0])) }
func (v valueSnapshot) bytes() []byte       { return v.encoded }
func (v valueSnapshot) proofPointer() *C.size_t {
	if len(v.duplicates) == 0 {
		return nil
	}
	return &v.duplicates[0]
}

// getView copies the record into aligned Go storage and validates it once.
// It blocks under the same pool lock as Get and does not imply cancellation.
func (p *Pool) getView(seq uint64) (*MessageView, error) {
	view, err := p.copyView(seq)
	if err != nil {
		return nil, err
	}
	if err := view.validate(); err != nil {
		return nil, err
	}
	return view, nil
}
func (view *MessageView) validate() error {
	if len(view.encoded) == 0 {
		return invalidArgumentError("empty encoded view")
	}
	if uintptr(unsafe.Pointer(&view.encoded[0]))%4 != 0 {
		words := make([]uint32, (len(view.encoded)+3)/4)
		aligned := unsafe.Slice((*byte)(unsafe.Pointer(&words[0])), len(view.encoded))
		copy(aligned, view.encoded)
		view.encoded = aligned
	}
	result := C.plsm_go_validate_result(view.pointer(), C.size_t(len(view.encoded)))
	runtime.KeepAlive(view)
	if result.status != 0 {
		return fromCError(result.error)
	}
	view.dataNode = result.data
	view.metaNode = result.meta
	proof := result.proof
	if proof.len != 0 {
		count := int(proof.len) / int(unsafe.Sizeof(C.size_t(0)))
		view.duplicates = make([]C.size_t, count)
		raw := unsafe.Slice((*byte)(unsafe.Pointer(proof.data)), int(proof.len))
		for i := range view.duplicates {
			if unsafe.Sizeof(C.size_t(0)) == 8 {
				view.duplicates[i] = C.size_t(binary.NativeEndian.Uint64(raw[i*8:]))
			} else {
				view.duplicates[i] = C.size_t(binary.NativeEndian.Uint32(raw[i*4:]))
			}
		}
	}
	if proof.len != 0 {
		C.plsm_go_free_buffer(proof)
	}
	return nil
}
func (p *Pool) copyView(seq uint64) (*MessageView, error) { return p.copyViewCapacity(seq, 256) }
func (p *Pool) copyViewCapacity(seq uint64, initialWords int) (*MessageView, error) {
	if p == nil {
		return nil, closedError("pool")
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.ptr == nil {
		return nil, closedError("pool")
	}
	words := make([]uint32, initialWords)
	var result C.plsm_go_get_result_t
	for {
		var destination *C.uint8_t
		if len(words) > 0 {
			destination = (*C.uint8_t)(unsafe.Pointer(&words[0]))
		}
		result = C.plsm_go_get_into(p.ptr, C.uint64_t(seq), destination, C.size_t(len(words)*4))
		runtime.KeepAlive(words)
		if result.status != 0 {
			return nil, fromCError(result.error)
		}
		if uint64(result.len) <= uint64(len(words))*4 {
			break
		}
		if uint64(result.len) > uint64(^uint(0)>>1)-3 {
			return nil, invalidArgumentError("record is too large")
		}
		words = make([]uint32, (int(result.len)+3)/4)
	}
	if result.len == 0 {
		return nil, &Error{Kind: ErrorCorrupt, Message: "empty encoded view"}
	}
	view := &MessageView{seq: seq, timestampNs: uint64(result.timestamp), valueSnapshot: valueSnapshot{encoded: unsafe.Slice((*byte)(unsafe.Pointer(&words[0])), int(result.len))}}
	return view, nil
}

// Time constructs the UTC time only when requested.
func (v *MessageView) Seq() uint64  { return v.seq }
func (v *MessageView) Time() uint64 { return v.timestampNs }

// Lite3 returns a mutable copy of the complete encoded snapshot.
func (v *MessageView) Lite3() []byte { return append([]byte(nil), v.bytes()...) }

// Data returns a borrowed reference to the data object.
func (v *MessageView) Data() Value {
	if v == nil || len(v.encoded) == 0 {
		return Value{}
	}
	return Value{owner: v.valueSnapshot, node: v.dataNode, kind: 7}
}

// Meta returns the complete stored metadata, including unknown fields.
func (v *MessageView) Meta() Value {
	if v == nil || len(v.encoded) == 0 {
		return Value{}
	}
	return Value{owner: v.valueSnapshot, node: v.metaNode, kind: 7}
}

// DataJSON formats the data object directly, without a native object tree.
func (v *MessageView) DataJSON() ([]byte, error) { return v.Data().JSON() }

// JSON formats the complete message envelope. The returned bytes are mutable.
func (v *MessageView) JSON() ([]byte, error) {
	if v == nil || len(v.encoded) == 0 {
		return nil, invalidArgumentError("missing view")
	}
	result := C.plsm_go_message_result(v.pointer(), C.size_t(len(v.encoded)), v.proofPointer(), C.size_t(len(v.duplicates)), v.dataNode, C.uint64_t(v.seq), C.uint64_t(v.timestampNs))
	runtime.KeepAlive(v)
	if result.status != 0 {
		return nil, fromCError(result.error)
	}
	return copyNativeJSON(result.buffer), nil
}

// Decode builds the requested ordinary Go value using encoding/json semantics.
func (v *MessageView) Decode(destination any) error { return v.Data().Decode(destination) }

// Selector prepares JSON Pointer paths relative to data for repeated reads.
// Close releases its small native path plan; messages and values need no Close.
type Selector struct {
	mu      sync.RWMutex
	pointer *C.plsm_selector_t
	count   int
}

func Prepare(paths ...string) (*Selector, error) {
	for _, path := range paths {
		if strings.ContainsRune(path, '\x00') {
			return nil, invalidArgumentError("selector path contains NUL")
		}
	}
	input, free := cStringArray(paths)
	defer free()
	var pointer *C.plsm_selector_t
	var cErr *C.plsm_error_t
	if C.plsm_selector_new(input, C.size_t(len(paths)), &pointer, &cErr) != 0 {
		return nil, fromCError(cErr)
	}
	selector := &Selector{pointer: pointer, count: len(paths)}
	runtime.SetFinalizer(selector, (*Selector).Close)
	return selector, nil
}
func (s *Selector) Close() {
	if s == nil {
		return
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.pointer != nil {
		C.plsm_selector_free(s.pointer)
		s.pointer = nil
	}
	runtime.SetFinalizer(s, nil)
}
func (s *Selector) read(v valueSnapshot, root C.size_t) ([]C.plsm_go_value, error) {
	if s == nil || len(v.encoded) == 0 {
		return nil, invalidArgumentError("selector and view are required")
	}
	s.mu.RLock()
	defer s.mu.RUnlock()
	if s.pointer == nil {
		return nil, closedError("selector")
	}
	slots := make([]C.plsm_go_value, s.count)
	var output *C.plsm_go_value
	if len(slots) > 0 {
		output = &slots[0]
	}
	result := C.plsm_go_select_result(v.pointer(), C.size_t(len(v.encoded)), v.proofPointer(), C.size_t(len(v.duplicates)), root, s.pointer, output, C.size_t(len(slots)))
	runtime.KeepAlive(v)
	runtime.KeepAlive(s)
	if result.status != 0 {
		return nil, fromCError(result.error)
	}
	return slots, nil
}
func (s *Selector) Read(v *MessageView) ([]Value, error) {
	if v == nil || len(v.encoded) == 0 {
		return nil, invalidArgumentError("view is required")
	}
	snapshot := v.valueSnapshot
	slots, err := s.read(snapshot, v.dataNode)
	if err != nil {
		return nil, err
	}
	values := make([]Value, len(slots))
	for i, slot := range slots {
		values[i] = Value{owner: snapshot, node: slot.node, kind: Kind(slot.kind), integer: int64(slot.integer), real: float64(slot.real), offset: int(slot.offset), length: int(slot.len)}
	}
	return values, nil
}

// Kind identifies a selected Lite3 value. Missing and null remain distinct.
type Kind uint32

const (
	KindMissing Kind = iota
	KindNull
	KindBool
	KindInt64
	KindFloat64
	KindString
	KindBytes
	KindObject
	KindArray
)

// Value retains its original immutable buffers through copied slice headers.
// Kind follows the C value kinds: missing,
// null, bool, integer, real, string, bytes, object and array (0 through 8).
type Value struct {
	owner          valueSnapshot
	node           C.size_t
	kind           Kind
	integer        int64
	real           float64
	offset, length int
}

func (v Value) Kind() Kind               { return v.kind }
func (v Value) Int64() (int64, bool)     { return v.integer, v.kind == 3 }
func (v Value) Float64() (float64, bool) { return v.real, v.kind == 4 }
func (v Value) Bool() (bool, bool)       { return v.integer != 0, v.kind == 2 }
func (v Value) String() (string, bool) {
	if v.kind != 5 {
		return "", false
	}
	return string(v.owner.bytes()[v.offset : v.offset+v.length]), true
}
func (v Value) Bytes() ([]byte, bool) {
	if v.kind != 6 {
		return nil, false
	}
	output := make([]byte, v.length)
	copy(output, v.owner.bytes()[v.offset:v.offset+v.length])
	return output, true
}
func (v Value) JSON() ([]byte, error) {
	if len(v.owner.encoded) == 0 || v.kind == 0 {
		return nil, invalidArgumentError("missing value")
	}
	result := C.plsm_go_json_result(v.owner.pointer(), C.size_t(len(v.owner.encoded)), v.node)
	runtime.KeepAlive(v.owner)
	if result.status != 0 {
		return nil, fromCError(result.error)
	}
	return copyNativeJSON(result.buffer), nil
}
func (v Value) Decode(destination any) error {
	if v.kind == KindBytes {
		if target, ok := destination.(*[]byte); ok && target != nil {
			*target, _ = v.Bytes()
			return nil
		}
	}
	if len(v.owner.encoded) == 0 || v.kind == 0 {
		return invalidArgumentError("missing value")
	}
	if handled, err := v.decodeStruct(destination); handled {
		return err
	}
	raw, err := v.JSON()
	if err != nil {
		return err
	}
	return json.Unmarshal(raw, destination)
}

func copyNativeJSON(buffer C.plsm_buf_t) []byte {
	output := C.GoBytes(unsafe.Pointer(buffer.data), C.int(buffer.len))
	C.plsm_go_free_buffer(buffer)
	return output
}

// Struct plans contain only native field metadata; they retain no record bytes.
// The type cache owns them, just as encoding/json retains its type plans.
type structDecoder struct {
	pointer *C.plsm_go_decoder_t
	count   int
}

func prepareStructDecoder(selector *Selector, objects []byte) (*structDecoder, error) {
	result := C.plsm_go_prepare_decoder(selector.pointer, (*C.uint8_t)(unsafe.Pointer(&objects[0])), C.size_t(len(objects)))
	runtime.KeepAlive(selector)
	runtime.KeepAlive(objects)
	if result.status != 0 {
		return nil, fromCError(result.error)
	}
	decoder := &structDecoder{pointer: result.decoder, count: len(objects)}
	runtime.SetFinalizer(decoder, (*structDecoder).close)
	return decoder, nil
}
func (d *structDecoder) close() {
	if d.pointer != nil {
		C.plsm_go_decoder_free(d.pointer)
		d.pointer = nil
	}
	runtime.SetFinalizer(d, nil)
}
func (d *structDecoder) read(view valueSnapshot, root C.size_t) ([]C.plsm_go_value, bool, error) {
	slots := make([]C.plsm_go_value, d.count)
	result := C.plsm_go_decode_result(view.pointer(), C.size_t(len(view.encoded)), view.proofPointer(), C.size_t(len(view.duplicates)), root, d.pointer, &slots[0], C.size_t(len(slots)))
	runtime.KeepAlive(view)
	runtime.KeepAlive(d)
	if result.status == 1 {
		return nil, false, nil
	}
	if result.status != 0 {
		return nil, false, fromCError(result.error)
	}
	return slots, true, nil
}

// MarshalJSON lets standard Go JSON operations format views directly.
func (v Value) MarshalJSON() ([]byte, error)        { return v.JSON() }
func (v *MessageView) MarshalJSON() ([]byte, error) { return v.JSON() }
