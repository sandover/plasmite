/*
Purpose: Go bindings for the libplasmite C ABI (major 2).
Key Exports: Client, Pool, Stream, Durability, Error.
Role: Minimal, ergonomic wrapper around include/plasmite.h for Go users.
Invariants: Caller must Close resources; JSON bytes in/out; errors returned as Go error.
Notes: Uses cgo and links to -lplasmite; caller configures library search path.
*/
package local

/*
#cgo pkg-config: plasmite
#cgo CFLAGS: -I${SRCDIR}/../../include
#cgo darwin LDFLAGS: -lplasmite.2
#cgo linux LDFLAGS: -l:libplasmite.so.2
#cgo windows LDFLAGS: -lplasmite2
#include "plasmite.h"
#include <stdlib.h>
*/
import "C"

import (
	"context"
	"fmt"
	"io"
	"os"
	"os/user"
	"path/filepath"
	"runtime"
	"sync"
	"unsafe"

	"github.com/sandover/plasmite/bindings/go/api"
)

type ErrorKind = api.ErrorKind

const (
	ErrorInternal      ErrorKind = api.ErrorInternal
	ErrorUsage         ErrorKind = api.ErrorUsage
	ErrorNotFound      ErrorKind = api.ErrorNotFound
	ErrorAlreadyExists ErrorKind = api.ErrorAlreadyExists
	ErrorBusy          ErrorKind = api.ErrorBusy
	ErrorPermission    ErrorKind = api.ErrorPermission
	ErrorCorrupt       ErrorKind = api.ErrorCorrupt
	ErrorIO            ErrorKind = api.ErrorIO
	ErrorRetentionGap  ErrorKind = api.ErrorRetentionGap
)

type Error = api.Error

// Client serializes native handle access, including Close.
// A Client must not be copied.
type Client struct {
	mu  sync.Mutex
	ptr *C.plsm_client_t
}

// Pool serializes native handle access, including Close and stream creation.
// A Pool must not be copied; streams own separate handles.
type Pool struct {
	mu  sync.Mutex
	ptr *C.plsm_pool_t
}

// Stream serializes native reads and Close. A Stream must not be copied.
// Pass a context to Next to bound its wait. Close stops subsequent reads.
type Stream struct {
	mu  sync.Mutex
	ptr *C.plsm_stream_t
}

// Lite3Stream serializes native reads and Close. A Lite3Stream must not be copied.
// Pass a context to Next to bound its wait. Close stops subsequent reads.
type Lite3Stream struct {
	mu  sync.Mutex
	ptr *C.plsm_lite3_stream_t
}

type Durability = api.Durability

const (
	DurabilityFast  Durability = api.DurabilityFast
	DurabilityFlush Durability = api.DurabilityFlush
)

type PoolRef = api.PoolRef

func PoolRefName(name string) PoolRef { return api.PoolRefName(name) }

func PoolRefPath(path string) PoolRef { return api.PoolRefPath(path) }

func PoolRefURI(uri string) PoolRef { return api.PoolRefURI(uri) }

type Lite3Frame = api.Lite3Frame

type FeedOption = api.FeedOption

func WithDurability(d Durability) FeedOption { return api.WithDurability(d) }

var (
	_ api.Pool = (*Pool)(nil)
)

func NewClient(poolDir string) (*Client, error) {
	if major := C.plsm_abi_major(); major != 2 {
		return nil, &Error{Kind: ErrorUsage, Message: fmt.Sprintf("Plasmite requires ABI major 2; found %d", major)}
	}
	if poolDir == "" {
		return nil, invalidArgumentError("poolDir is required")
	}
	cPoolDir := C.CString(poolDir)
	defer C.free(unsafe.Pointer(cPoolDir))

	var cClient *C.plsm_client_t
	var cErr *C.plsm_error_t
	rc := C.plsm_client_new(cPoolDir, &cClient, &cErr)
	if rc != 0 {
		return nil, fromCError(cErr)
	}
	return &Client{ptr: cClient}, nil
}

func NewDefaultClient() (*Client, error) {
	directory, err := DefaultPoolDir()
	if err != nil {
		return nil, err
	}
	return NewClient(directory)
}

func DefaultPoolDir() (string, error) {
	var home string
	if runtime.GOOS == "windows" {
		home, _ = os.UserHomeDir()
		if !filepath.IsAbs(home) {
			if account, err := user.Current(); err == nil {
				home = account.HomeDir
			}
		}
	} else {
		home = os.Getenv("HOME")
		if !filepath.IsAbs(home) {
			if account, err := user.Current(); err == nil {
				home = account.HomeDir
			}
		}
	}
	if !filepath.IsAbs(home) {
		return "", invalidArgumentError("cannot determine the user home directory; supply an explicit pool directory")
	}
	return filepath.Join(home, ".plasmite", "pools"), nil
}

func (c *Client) Close() {
	if c == nil {
		return
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.ptr == nil {
		return
	}
	C.plsm_client_free(c.ptr)
	c.ptr = nil
}

func (c *Client) CreatePool(ref PoolRef, sizeBytes uint64) (*Pool, error) {
	if c == nil {
		return nil, closedError("client")
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.ptr == nil {
		return nil, closedError("client")
	}
	if ref == "" {
		return nil, invalidArgumentError("pool ref is required")
	}
	if sizeBytes == 0 {
		sizeBytes = api.DefaultPoolSizeBytes
	}
	cRef := C.CString(string(ref))
	defer C.free(unsafe.Pointer(cRef))

	var cPool *C.plsm_pool_t
	var cErr *C.plsm_error_t
	rc := C.plsm_pool_create(c.ptr, cRef, C.uint64_t(sizeBytes), &cPool, &cErr)
	if rc != 0 {
		return nil, fromCError(cErr)
	}
	return &Pool{ptr: cPool}, nil
}

func (c *Client) OpenPool(ref PoolRef, opts ...api.OpenOption) (*Pool, error) {
	if c == nil {
		return nil, closedError("client")
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.ptr == nil {
		return nil, closedError("client")
	}
	if ref == "" {
		return nil, invalidArgumentError("pool ref is required")
	}
	cRef := C.CString(string(ref))
	defer C.free(unsafe.Pointer(cRef))

	var cPool *C.plsm_pool_t
	var cErr *C.plsm_error_t
	var cfg api.OpenConfig
	for _, opt := range opts {
		opt(&cfg)
	}
	var create C.uint32_t
	if cfg.Create {
		create = 1
	}
	rc := C.plsm_pool_open(c.ptr, cRef, create, &cPool, &cErr)
	if rc != 0 {
		return nil, fromCError(cErr)
	}
	return &Pool{ptr: cPool}, nil
}

func (p *Pool) Close() {
	if p == nil {
		return
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.ptr == nil {
		return
	}
	C.plsm_pool_free(p.ptr)
	p.ptr = nil
}

func (p *Pool) getJSON(seq uint64) ([]byte, error) {
	stream, err := p.Fetch(Since(seq))
	if err != nil {
		return nil, err
	}
	defer stream.Close()
	message, err := stream.Next(context.Background())
	if err == io.EOF {
		return nil, &Error{Kind: ErrorNotFound, Message: "message not found", Seq: &seq}
	}
	if err != nil {
		return nil, err
	}
	return message.Raw, nil
}

func (p *Pool) getLite3(seq uint64) (*Lite3Frame, error) {
	stream, err := p.FetchLite3(Since(seq))
	if err != nil {
		return nil, err
	}
	defer stream.Close()
	frame, err := stream.Next(context.Background())
	if err == io.EOF {
		return nil, &Error{Kind: ErrorNotFound, Message: "message not found", Seq: &seq}
	}
	return frame, err
}

func (s *Stream) Close() {
	if s == nil {
		return
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.ptr == nil {
		return
	}
	C.plsm_stream_free(s.ptr)
	s.ptr = nil
}

func (s *Lite3Stream) Close() {
	if s == nil {
		return
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.ptr == nil {
		return
	}
	C.plsm_lite3_stream_free(s.ptr)
	s.ptr = nil
}

func (s *Stream) nextWait(wait uint64) ([]byte, int, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.ptr == nil {
		return nil, -1, closedError("stream")
	}
	var buf C.plsm_buf_t
	var err *C.plsm_error_t
	status := int(C.plsm_stream_next_wait(s.ptr, C.uint64_t(wait), &buf, &err))
	if status < 0 {
		return nil, status, fromCError(err)
	}
	if status == 1 {
		return copyAndFreeBuf(&buf), status, nil
	}
	return nil, status, nil
}

func (s *Lite3Stream) nextWait(wait uint64) (*Lite3Frame, int, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.ptr == nil {
		return nil, -1, closedError("stream")
	}
	var frame C.plsm_lite3_frame_t
	var err *C.plsm_error_t
	status := int(C.plsm_lite3_stream_next_wait(s.ptr, C.uint64_t(wait), &frame, &err))
	if status < 0 {
		return nil, status, fromCError(err)
	}
	if status == 1 {
		return copyAndFreeLite3Frame(&frame), status, nil
	}
	return nil, status, nil
}

func copyAndFreeBuf(buf *C.plsm_buf_t) []byte {
	if buf == nil {
		return nil
	}
	defer C.plsm_buf_free(buf)
	if buf.data == nil || buf.len == 0 {
		return nil
	}
	data := C.GoBytes(unsafe.Pointer(buf.data), C.int(buf.len))
	return data
}

func copyAndFreeLite3Frame(frame *C.plsm_lite3_frame_t) *Lite3Frame {
	if frame == nil {
		return nil
	}
	var payload []byte
	if frame.payload.data != nil && frame.payload.len != 0 {
		payload = C.GoBytes(unsafe.Pointer(frame.payload.data), C.int(frame.payload.len))
	}
	out := &Lite3Frame{
		Seq:     uint64(frame.seq),
		Time:    uint64(frame.time),
		Flags:   uint32(frame.flags),
		Payload: payload,
	}
	C.plsm_lite3_frame_free(frame)
	return out
}

func fromCError(cErr *C.plsm_error_t) *Error {
	if cErr == nil {
		return &Error{Kind: ErrorInternal, Message: "unknown error"}
	}
	defer C.plsm_error_free(cErr)

	err := &Error{Kind: ErrorKind(cErr.kind), Missing: uint64(cErr.missing), NextSeq: uint64(cErr.next_seq)}
	if cErr.message != nil {
		err.Message = C.GoString(cErr.message)
	}
	if cErr.path != nil {
		err.Path = C.GoString(cErr.path)
	}
	if cErr.has_seq != 0 {
		seq := uint64(cErr.seq)
		err.Seq = &seq
	}
	if cErr.has_offset != 0 {
		offset := uint64(cErr.offset)
		err.Offset = &offset
	}
	if err.Message == "" {
		err.Message = "unknown error"
	}
	return err
}

func cStringArray(values []string) (**C.char, func()) {
	if len(values) == 0 {
		return nil, func() {}
	}
	// One C allocation owns all tag bytes. The pointer array contains only C
	// pointers and remains pinned for the duration of the native call.
	var data []byte
	for _, value := range values {
		data = append(data, value...)
		data = append(data, 0)
	}
	base := C.CBytes(data)
	cValues := make([]*C.char, len(values))
	offset := 0
	for i, value := range values {
		cValues[i] = (*C.char)(unsafe.Add(base, offset))
		offset += len(value) + 1
	}
	cleanup := func() { C.free(base) }
	return (**C.char)(unsafe.Pointer(&cValues[0])), cleanup
}

func closedError(target string) error {
	return api.ClosedError(target)
}

func invalidArgumentError(message string) error {
	return api.InvalidArgumentError(message)
}
