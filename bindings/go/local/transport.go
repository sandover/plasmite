package local

/*
#include "borrowed.h"
*/
import "C"

import (
	"github.com/sandover/plasmite/bindings/go/v2/api"
	"github.com/sandover/plasmite/bindings/go/v2/internal/nativeview"
	"runtime"
	"unsafe"
)

// AdoptMessage transfers a checked snapshot from a sibling Go transport. The
// parameter type is internal to the Go binding.
func AdoptMessage(snapshot nativeview.Snapshot) *api.Message {
	return adoptSnapshot(snapshot).message()
}

// AdoptSnapshot is transport glue. Its internal parameter type prevents external
// callers from claiming that arbitrary mutable bytes have a validation proof.
func adoptSnapshot(snapshot nativeview.Snapshot) *messageView {
	proof := snapshot.Duplicates()
	var duplicates []C.size_t
	if len(proof) > 0 {
		duplicates = unsafe.Slice((*C.size_t)(unsafe.Pointer(&proof[0])), len(proof))
	}
	data, meta := snapshot.Offsets()
	seq, timestamp := snapshot.Identity()
	return &messageView{seq: seq, timeNS: timestamp, valueSnapshot: valueSnapshot{encoded: snapshot.Bytes(), duplicates: duplicates}, dataNode: C.size_t(data), metaNode: C.size_t(meta)}
}

// forwardView shares immutable Go storage/proof and assigns destination identity.
func (p *Pool) forwardView(source *messageView, durability Durability) (*messageView, error) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.ptr == nil {
		return nil, closedError("pool")
	}
	result := C.plsm_go_forward_view(p.ptr, source.pointer(), C.size_t(len(source.encoded)), C.uint32_t(durability))
	runtime.KeepAlive(source)
	if result.status != 0 {
		return nil, fromCError(result.error)
	}
	forwarded := *source
	forwarded.seq = uint64(result.seq)
	forwarded.timeNS = uint64(result.timestamp)
	return &forwarded, nil
}
