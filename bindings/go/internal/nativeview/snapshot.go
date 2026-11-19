// Package nativeview transfers checked transport owners into private Go storage.
// Only binding packages can import this implementation seam.
package nativeview

/*
#cgo pkg-config: plasmite
#cgo CFLAGS: -I${SRCDIR}/../../../../include
#cgo darwin LDFLAGS: -lplasmite.2
#cgo linux LDFLAGS: -l:libplasmite.so.2
#cgo windows LDFLAGS: -lplasmite2
#include <stdlib.h>
#include "owner.h"
*/
import "C"

import (
	"github.com/sandover/plasmite/bindings/go/v2/api"
	"runtime"
	"slices"
	"unsafe"
)

// TagSpans preserves embedded NUL and keeps each span valid until cleanup.
func TagSpans(tags []string) (unsafe.Pointer, func()) {
	if len(tags) == 0 {
		return nil, func() {}
	}
	var bytes []byte
	for _, tag := range tags {
		bytes = append(bytes, tag...)
	}
	storage := C.CBytes(append(bytes, 0))
	spans := make([]C.plsm_text_t, len(tags))
	offset := 0
	for i, tag := range tags {
		spans[i].data = (*C.uint8_t)(unsafe.Add(storage, offset))
		spans[i].len = C.size_t(len(tag))
		offset += len(tag)
	}
	var pin runtime.Pinner
	pin.Pin(&spans[0])
	return unsafe.Pointer(&spans[0]), func() {
		pin.Unpin()
		C.free(storage)
	}
}

// Snapshot contains only private Go allocations and relative checked offsets.
// Binding code must keep Bytes and Duplicates immutable after transfer.
type Snapshot struct {
	bytes       []byte
	duplicates  []uintptr
	data, meta  uintptr
	seq, timeNS uint64
}

// Binding packages alone can read these buffers; keep them immutable.
func (s Snapshot) Bytes() []byte               { return s.bytes }
func (s Snapshot) Duplicates() []uintptr       { return s.duplicates }
func (s Snapshot) Offsets() (uintptr, uintptr) { return s.data, s.meta }
func (s Snapshot) Identity() (uint64, uint64)  { return s.seq, s.timeNS }

// CopyAndRelease consumes one retained native owner, including on failure.
// It copies the already-checked encoding/proof without validating it again.
func CopyAndRelease(pointer unsafe.Pointer) (Snapshot, error) {
	owner := (*C.plsm_message_view_t)(pointer)
	if owner == nil {
		return Snapshot{}, &api.Error{Kind: api.ErrorInternal, Message: "missing view owner"}
	}
	defer C.plsm_view_release(owner)
	info := C.plsm_go_view_info(owner)
	if info.bytes == nil || info.len == 0 || uint64(info.len) > uint64(^uint(0)>>1)-3 {
		return Snapshot{}, &api.Error{Kind: api.ErrorInternal, Message: "invalid native view size"}
	}
	words := make([]uint32, (int(info.len)+3)/4)
	encoded := unsafe.Slice((*byte)(unsafe.Pointer(&words[0])), int(info.len))
	copy(encoded, unsafe.Slice((*byte)(unsafe.Pointer(info.bytes)), int(info.len)))
	result := Snapshot{bytes: encoded, data: uintptr(info.data_node), meta: uintptr(info.meta_node), seq: uint64(info.seq), timeNS: uint64(info.timestamp_ns)}
	if info.duplicate_count != 0 {
		result.duplicates = make([]uintptr, int(info.duplicate_count))
		copied := C.plsm_go_view_duplicates(owner, (*C.size_t)(unsafe.Pointer(&result.duplicates[0])), info.duplicate_count)
		runtime.KeepAlive(result.duplicates)
		if copied != info.duplicate_count {
			return Snapshot{}, &api.Error{Kind: api.ErrorInternal, Message: "inconsistent native proof"}
		}
		slices.Sort(result.duplicates)
	}
	return result, nil
}
