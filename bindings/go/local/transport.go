package local

/*
#include "borrowed.h"
*/
import "C"

import (
	"github.com/sandover/plasmite/bindings/go/internal/nativeview"
	"runtime"
	"unsafe"
)

// AdoptSnapshot is transport glue. Its internal parameter type prevents external
// callers from claiming that arbitrary mutable bytes have a validation proof.
func AdoptSnapshot(snapshot nativeview.Snapshot) *MessageView {
	proof := snapshot.Duplicates()
	var duplicates []C.size_t
	if len(proof) > 0 {
		duplicates = unsafe.Slice((*C.size_t)(unsafe.Pointer(&proof[0])), len(proof))
	}
	data, meta := snapshot.Offsets()
	seq, timestamp := snapshot.Identity()
	return &MessageView{seq: seq, timestampNs: timestamp, encoded: snapshot.Bytes(), dataNode: C.size_t(data), metaNode: C.size_t(meta), duplicates: duplicates}
}

// forwardView shares immutable Go storage/proof and assigns destination identity.
func (p *Pool) forwardView(source *MessageView, durability Durability) (*MessageView, error) {
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
	forwarded.timestampNs = uint64(result.timestamp)
	return &forwarded, nil
}
