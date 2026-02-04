/*
Purpose: Go bindings for the libplasmite C ABI (v0).
Key Exports: Client, Pool, Stream, Durability, Error.
Role: Minimal, ergonomic wrapper around include/plasmite.h for Go users.
Invariants: Caller must Close resources; JSON bytes in/out; errors returned as Go error.
Notes: Uses cgo and links to -lplasmite; caller configures library search path.
*/
package plasmite

/*
#cgo CFLAGS: -I${SRCDIR}/../../include
#cgo LDFLAGS: -lplasmite
#include "plasmite.h"
#include <stdlib.h>
*/
import "C"

import (
	"errors"
	"fmt"
	"runtime"
	"unsafe"
)

type ErrorKind int32

const (
	ErrorInternal      ErrorKind = C.PLSM_ERROR_INTERNAL
	ErrorUsage         ErrorKind = C.PLSM_ERROR_USAGE
	ErrorNotFound      ErrorKind = C.PLSM_ERROR_NOT_FOUND
	ErrorAlreadyExists ErrorKind = C.PLSM_ERROR_ALREADY_EXISTS
	ErrorBusy          ErrorKind = C.PLSM_ERROR_BUSY
	ErrorPermission    ErrorKind = C.PLSM_ERROR_PERMISSION
	ErrorCorrupt       ErrorKind = C.PLSM_ERROR_CORRUPT
	ErrorIO            ErrorKind = C.PLSM_ERROR_IO
)

type Error struct {
	Kind    ErrorKind
	Message string
	Path    string
	Seq     *uint64
	Offset  *uint64
}

func (e *Error) Error() string {
	if e == nil {
		return "plasmite: <nil error>"
	}
	if e.Path != "" {
		return fmt.Sprintf("plasmite: %s (%s)", e.Message, e.Path)
	}
	return fmt.Sprintf("plasmite: %s", e.Message)
}

type Durability uint32

const (
	DurabilityFast  Durability = 0
	DurabilityFlush Durability = 1
)

type Client struct {
	ptr *C.plsm_client_t
}

type Pool struct {
	ptr *C.plsm_pool_t
}

type Stream struct {
	ptr *C.plsm_stream_t
}

func NewClient(poolDir string) (*Client, error) {
	if poolDir == "" {
		return nil, errors.New("plasmite: poolDir is required")
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

func (c *Client) Close() {
	if c == nil || c.ptr == nil {
		return
	}
	C.plsm_client_free(c.ptr)
	c.ptr = nil
}

func (c *Client) CreatePool(ref string, sizeBytes uint64) (*Pool, error) {
	if c == nil || c.ptr == nil {
		return nil, errors.New("plasmite: client is closed")
	}
	if ref == "" {
		return nil, errors.New("plasmite: pool ref is required")
	}
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))

	var cPool *C.plsm_pool_t
	var cErr *C.plsm_error_t
	rc := C.plsm_pool_create(c.ptr, cRef, C.uint64_t(sizeBytes), &cPool, &cErr)
	if rc != 0 {
		return nil, fromCError(cErr)
	}
	return &Pool{ptr: cPool}, nil
}

func (c *Client) OpenPool(ref string) (*Pool, error) {
	if c == nil || c.ptr == nil {
		return nil, errors.New("plasmite: client is closed")
	}
	if ref == "" {
		return nil, errors.New("plasmite: pool ref is required")
	}
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))

	var cPool *C.plsm_pool_t
	var cErr *C.plsm_error_t
	rc := C.plsm_pool_open(c.ptr, cRef, &cPool, &cErr)
	if rc != 0 {
		return nil, fromCError(cErr)
	}
	return &Pool{ptr: cPool}, nil
}

func (p *Pool) Close() {
	if p == nil || p.ptr == nil {
		return
	}
	C.plsm_pool_free(p.ptr)
	p.ptr = nil
}

func (p *Pool) AppendJSON(payload []byte, descrips []string, durability Durability) ([]byte, error) {
	if p == nil || p.ptr == nil {
		return nil, errors.New("plasmite: pool is closed")
	}
	if len(payload) == 0 {
		return nil, errors.New("plasmite: payload is required")
	}
	cPayload := (*C.uint8_t)(unsafe.Pointer(&payload[0]))
	cLen := C.size_t(len(payload))

	cDescrips, cleanup := cStringArray(descrips)
	defer cleanup()

	var cBuf C.plsm_buf_t
	var cErr *C.plsm_error_t
	rc := C.plsm_pool_append_json(
		p.ptr,
		cPayload,
		cLen,
		cDescrips,
		C.size_t(len(descrips)),
		C.uint32_t(durability),
		&cBuf,
		&cErr,
	)
	runtime.KeepAlive(descrips)
	if rc != 0 {
		return nil, fromCError(cErr)
	}
	return copyAndFreeBuf(&cBuf), nil
}

func (p *Pool) GetJSON(seq uint64) ([]byte, error) {
	if p == nil || p.ptr == nil {
		return nil, errors.New("plasmite: pool is closed")
	}
	var cBuf C.plsm_buf_t
	var cErr *C.plsm_error_t
	rc := C.plsm_pool_get_json(p.ptr, C.uint64_t(seq), &cBuf, &cErr)
	if rc != 0 {
		return nil, fromCError(cErr)
	}
	return copyAndFreeBuf(&cBuf), nil
}

func (p *Pool) OpenStream(sinceSeq *uint64, maxMessages *uint64, timeoutMs *uint64) (*Stream, error) {
	if p == nil || p.ptr == nil {
		return nil, errors.New("plasmite: pool is closed")
	}
	var sinceVal C.uint64_t
	var hasSince C.uint32_t
	if sinceSeq != nil {
		sinceVal = C.uint64_t(*sinceSeq)
		hasSince = 1
	}
	var maxVal C.uint64_t
	var hasMax C.uint32_t
	if maxMessages != nil {
		maxVal = C.uint64_t(*maxMessages)
		hasMax = 1
	}
	var timeoutVal C.uint64_t
	var hasTimeout C.uint32_t
	if timeoutMs != nil {
		timeoutVal = C.uint64_t(*timeoutMs)
		hasTimeout = 1
	}

	var cStream *C.plsm_stream_t
	var cErr *C.plsm_error_t
	rc := C.plsm_stream_open(
		p.ptr,
		sinceVal,
		hasSince,
		maxVal,
		hasMax,
		timeoutVal,
		hasTimeout,
		&cStream,
		&cErr,
	)
	if rc != 0 {
		return nil, fromCError(cErr)
	}
	return &Stream{ptr: cStream}, nil
}

func (s *Stream) NextJSON() ([]byte, error) {
	if s == nil || s.ptr == nil {
		return nil, errors.New("plasmite: stream is closed")
	}
	var cBuf C.plsm_buf_t
	var cErr *C.plsm_error_t
	rc := C.plsm_stream_next(s.ptr, &cBuf, &cErr)
	if rc != 0 {
		return nil, fromCError(cErr)
	}
	return copyAndFreeBuf(&cBuf), nil
}

func (s *Stream) Close() {
	if s == nil || s.ptr == nil {
		return
	}
	C.plsm_stream_free(s.ptr)
	s.ptr = nil
}

func copyAndFreeBuf(buf *C.plsm_buf_t) []byte {
	if buf == nil || buf.data == nil || buf.len == 0 {
		return nil
	}
	data := C.GoBytes(unsafe.Pointer(buf.data), C.int(buf.len))
	C.plsm_buf_free(buf)
	return data
}

func fromCError(cErr *C.plsm_error_t) *Error {
	if cErr == nil {
		return &Error{Kind: ErrorInternal, Message: "unknown error"}
	}
	defer C.plsm_error_free(cErr)

	err := &Error{Kind: ErrorKind(cErr.kind)}
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
	cValues := make([]*C.char, len(values))
	for i, value := range values {
		cValues[i] = C.CString(value)
	}
	cleanup := func() {
		for _, value := range cValues {
			C.free(unsafe.Pointer(value))
		}
	}
	return (**C.char)(unsafe.Pointer(&cValues[0])), cleanup
}
