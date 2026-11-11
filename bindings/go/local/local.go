/*
Purpose: Go bindings for the libplasmite C ABI (v0).
Key Exports: Client, Pool, Stream, TailOptions, Durability, Error.
Role: Minimal, ergonomic wrapper around include/plasmite.h for Go users.
Invariants: Caller must Close resources; JSON bytes in/out; errors returned as Go error.
Notes: Uses cgo and links to -lplasmite; caller configures library search path.
*/
package local

/*
#cgo pkg-config: plasmite
#cgo CFLAGS: -I${SRCDIR}/../../include
#cgo LDFLAGS: -lplasmite
#include "plasmite.h"
#include <stdlib.h>
*/
import "C"

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"sync"
	"time"
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

var (
	ErrClosed          = api.ErrClosed
	ErrInvalidArgument = api.ErrInvalidArgument
)

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
// Close waits for an active read; use a finite stream timeout for bounded shutdown.
type Stream struct {
	mu  sync.Mutex
	ptr *C.plsm_stream_t
}

// Lite3Stream serializes native reads and Close. A Lite3Stream must not be copied.
// Close waits for an active read; use a finite stream timeout for bounded shutdown.
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

type TailOptions = api.TailOptions

type AppendOption = api.AppendOption

func WithDurability(d Durability) AppendOption { return api.WithDurability(d) }

var (
	_ api.Client      = (*Client)(nil)
	_ api.Pool        = (*Pool)(nil)
	_ api.Stream      = (*Stream)(nil)
	_ api.Lite3Stream = (*Lite3Stream)(nil)
)

func NewClient(poolDir string) (*Client, error) {
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
	return NewClient(DefaultPoolDir())
}

func DefaultPoolDir() string {
	home, err := os.UserHomeDir()
	if err != nil || home == "" {
		return filepath.Join(os.TempDir(), "plasmite", "pools")
	}
	return filepath.Join(home, ".plasmite", "pools")
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

func (c *Client) CreatePool(ref PoolRef, sizeBytes uint64) (api.Pool, error) {
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

func (c *Client) OpenPool(ref PoolRef) (api.Pool, error) {
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
	rc := C.plsm_pool_open(c.ptr, cRef, &cPool, &cErr)
	if rc != 0 {
		return nil, fromCError(cErr)
	}
	return &Pool{ptr: cPool}, nil
}

func (c *Client) Pool(ref PoolRef, sizeBytes uint64) (api.Pool, error) {
	pool, err := c.OpenPool(ref)
	if err == nil {
		return pool, nil
	}
	var perr *Error
	if errors.As(err, &perr) && perr.Kind == ErrorNotFound {
		return c.CreatePool(ref, sizeBytes)
	}
	return nil, err
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

func (p *Pool) AppendJSON(payload []byte, tags []string, durability Durability) ([]byte, error) {
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
	cPayload := (*C.uint8_t)(unsafe.Pointer(&payload[0]))
	cLen := C.size_t(len(payload))

	cTags, cleanup := cStringArray(tags)
	defer cleanup()

	var cBuf C.plsm_buf_t
	var cErr *C.plsm_error_t
	rc := C.plsm_pool_append_json(
		p.ptr,
		cPayload,
		cLen,
		cTags,
		C.size_t(len(tags)),
		C.uint32_t(durability),
		&cBuf,
		&cErr,
	)
	runtime.KeepAlive(tags)
	if rc != 0 {
		return nil, fromCError(cErr)
	}
	return copyAndFreeBuf(&cBuf), nil
}

func (p *Pool) Append(value any, tags []string, opts ...AppendOption) (*api.Message, error) {
	payload, err := json.Marshal(value)
	if err != nil {
		return nil, fmt.Errorf("plasmite: marshal payload: %w", err)
	}
	cfg := api.ApplyAppendOptions(opts...)
	raw, err := p.AppendJSON(payload, tags, cfg.Durability)
	if err != nil {
		return nil, err
	}
	return api.DecodeMessage(raw)
}

// AppendLite3 appends a pre-encoded Lite3 payload without JSON encoding.
func (p *Pool) AppendLite3(payload []byte, durability Durability) (uint64, error) {
	if p == nil {
		return 0, closedError("pool")
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.ptr == nil {
		return 0, closedError("pool")
	}
	if len(payload) == 0 {
		return 0, invalidArgumentError("payload is required")
	}
	cPayload := (*C.uint8_t)(unsafe.Pointer(&payload[0]))
	cLen := C.size_t(len(payload))

	var cSeq C.uint64_t
	var cErr *C.plsm_error_t
	rc := C.plsm_pool_append_lite3(
		p.ptr,
		cPayload,
		cLen,
		C.uint32_t(durability),
		&cSeq,
		&cErr,
	)
	runtime.KeepAlive(payload)
	if rc != 0 {
		return 0, fromCError(cErr)
	}
	return uint64(cSeq), nil
}

func (p *Pool) GetJSON(seq uint64) ([]byte, error) {
	if p == nil {
		return nil, closedError("pool")
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.ptr == nil {
		return nil, closedError("pool")
	}
	var cBuf C.plsm_buf_t
	var cErr *C.plsm_error_t
	rc := C.plsm_pool_get_json(p.ptr, C.uint64_t(seq), &cBuf, &cErr)
	if rc != 0 {
		return nil, fromCError(cErr)
	}
	return copyAndFreeBuf(&cBuf), nil
}

func (p *Pool) Get(seq uint64) (*api.Message, error) {
	raw, err := p.GetJSON(seq)
	if err != nil {
		return nil, err
	}
	return api.DecodeMessage(raw)
}

// GetLite3 returns the raw Lite3 payload and metadata for the given sequence.
func (p *Pool) GetLite3(seq uint64) (*Lite3Frame, error) {
	if p == nil {
		return nil, closedError("pool")
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.ptr == nil {
		return nil, closedError("pool")
	}
	var cFrame C.plsm_lite3_frame_t
	var cErr *C.plsm_error_t
	rc := C.plsm_pool_get_lite3(p.ptr, C.uint64_t(seq), &cFrame, &cErr)
	if rc != 0 {
		return nil, fromCError(cErr)
	}
	return copyAndFreeLite3Frame(&cFrame), nil
}

func (p *Pool) OpenStream(sinceSeq *uint64, maxMessages *uint64, timeoutMs *uint64) (api.Stream, error) {
	stream, err := p.openStream(sinceSeq, maxMessages, timeoutMs, false)
	if err != nil {
		return nil, err
	}
	return stream, nil
}

func (p *Pool) openStream(sinceSeq *uint64, maxMessages *uint64, timeoutMs *uint64, errorOnGap bool) (*Stream, error) {
	if p == nil {
		return nil, closedError("pool")
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.ptr == nil {
		return nil, closedError("pool")
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
	var rc C.int
	if errorOnGap {
		options := C.plsm_stream_options_t{
			struct_size:  C.uint32_t(C.sizeof_plsm_stream_options_t),
			gap_policy:   C.uint32_t(C.PLSM_GAP_ERROR),
			since_seq:    sinceVal,
			max_messages: maxVal,
			timeout_ms:   timeoutVal,
			has_since:    hasSince,
			has_max:      hasMax,
			has_timeout:  hasTimeout,
		}
		rc = C.plsm_stream_open_ex(p.ptr, &options, &cStream, &cErr)
	} else {
		rc = C.plsm_stream_open(
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
	}
	if rc != 0 {
		return nil, fromCError(cErr)
	}
	return &Stream{ptr: cStream}, nil
}

func (p *Pool) OpenLite3Stream(sinceSeq *uint64, maxMessages *uint64, timeoutMs *uint64) (api.Lite3Stream, error) {
	stream, err := p.openLite3Stream(sinceSeq, maxMessages, timeoutMs, false)
	if err != nil {
		return nil, err
	}
	return stream, nil
}

func (p *Pool) openLite3Stream(sinceSeq *uint64, maxMessages *uint64, timeoutMs *uint64, errorOnGap bool) (*Lite3Stream, error) {
	if p == nil {
		return nil, closedError("pool")
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.ptr == nil {
		return nil, closedError("pool")
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

	var cStream *C.plsm_lite3_stream_t
	var cErr *C.plsm_error_t
	var rc C.int
	if errorOnGap {
		options := C.plsm_stream_options_t{
			struct_size:  C.uint32_t(C.sizeof_plsm_stream_options_t),
			gap_policy:   C.uint32_t(C.PLSM_GAP_ERROR),
			since_seq:    sinceVal,
			max_messages: maxVal,
			timeout_ms:   timeoutVal,
			has_since:    hasSince,
			has_max:      hasMax,
			has_timeout:  hasTimeout,
		}
		rc = C.plsm_lite3_stream_open_ex(p.ptr, &options, &cStream, &cErr)
	} else {
		rc = C.plsm_lite3_stream_open(
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
	}
	if rc != 0 {
		return nil, fromCError(cErr)
	}
	return &Lite3Stream{ptr: cStream}, nil
}

func (s *Stream) NextJSON() ([]byte, error) {
	if s == nil {
		return nil, closedError("stream")
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.ptr == nil {
		return nil, closedError("stream")
	}
	var cBuf C.plsm_buf_t
	var cErr *C.plsm_error_t
	rc := C.plsm_stream_next(s.ptr, &cBuf, &cErr)
	switch rc {
	case 1:
		return copyAndFreeBuf(&cBuf), nil
	case 0:
		return nil, io.EOF
	default:
		return nil, fromCError(cErr)
	}
}

func (s *Lite3Stream) Next() (*Lite3Frame, error) {
	if s == nil {
		return nil, closedError("stream")
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.ptr == nil {
		return nil, closedError("stream")
	}
	var cFrame C.plsm_lite3_frame_t
	var cErr *C.plsm_error_t
	rc := C.plsm_lite3_stream_next(s.ptr, &cFrame, &cErr)
	switch rc {
	case 1:
		return copyAndFreeLite3Frame(&cFrame), nil
	case 0:
		return nil, io.EOF
	default:
		return nil, fromCError(cErr)
	}
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

func streamTimeoutMillis(timeout time.Duration) uint64 {
	if timeout <= 0 {
		timeout = time.Second
	}
	if timeout < time.Millisecond {
		return 1
	}
	return uint64(timeout.Milliseconds())
}

// tailPoolOpen preserves the high-level tail's Close behavior while its native
// stream retains an independent handle. Low-level streams outlive their Pool.
func (p *Pool) tailPoolOpen() bool {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.ptr != nil
}

// Tail streams matching JSON messages through one native reader. Bounded waits
// check context cancellation at least every 50 ms while the pool is idle.
func (p *Pool) Tail(ctx context.Context, opts TailOptions) (<-chan *api.Message, <-chan error) {
	out := make(chan *api.Message, bufferSize(opts.Buffer))
	errs := make(chan error, 1)
	go func() {
		defer close(out)
		defer close(errs)
		if opts.MaxMessages != nil && *opts.MaxMessages == 0 {
			return
		}
		limit := opts.MaxMessages
		if len(opts.Tags) > 0 {
			limit = nil
		}
		stream, err := p.openStream(opts.SinceSeq, limit, nil, opts.ErrorOnGap)
		if err != nil {
			errs <- err
			return
		}
		defer stream.Close()
		var delivered uint64
		wait := min(streamTimeoutMillis(opts.Timeout), uint64(50))
		for {
			if ctx.Err() != nil {
				errs <- ctx.Err()
				return
			}
			if !p.tailPoolOpen() {
				errs <- closedError("pool")
				return
			}
			raw, status, err := stream.nextWait(wait)
			if err != nil {
				errs <- err
				return
			}
			if status == 2 || status == 3 {
				return
			}
			if status == 0 {
				continue
			}
			msg, err := api.DecodeMessage(raw)
			if err != nil {
				errs <- err
				return
			}
			if messageHasTags(msg, opts.Tags) {
				select {
				case out <- msg:
					delivered++
				case <-ctx.Done():
					errs <- ctx.Err()
					return
				}
			}
			if (opts.MaxMessages != nil && delivered >= *opts.MaxMessages) || msg.Seq == ^uint64(0) {
				return
			}
		}
	}()
	return out, errs
}

// TailLite3 streams raw frames through one native reader. Bounded waits retain
// its checkpoint while checking context cancellation and Pool.Close.
func (p *Pool) TailLite3(ctx context.Context, opts TailOptions) (<-chan *Lite3Frame, <-chan error) {
	out := make(chan *Lite3Frame, bufferSize(opts.Buffer))
	errs := make(chan error, 1)
	go func() {
		defer close(out)
		defer close(errs)
		if opts.MaxMessages != nil && *opts.MaxMessages == 0 {
			return
		}
		stream, err := p.openLite3Stream(opts.SinceSeq, opts.MaxMessages, nil, opts.ErrorOnGap)
		if err != nil {
			errs <- err
			return
		}
		defer stream.Close()
		wait := min(streamTimeoutMillis(opts.Timeout), uint64(50))
		for {
			if ctx.Err() != nil {
				errs <- ctx.Err()
				return
			}
			if !p.tailPoolOpen() {
				errs <- closedError("pool")
				return
			}
			frame, status, err := stream.nextWait(wait)
			if err != nil {
				errs <- err
				return
			}
			if status == 2 || status == 3 {
				return
			}
			if status == 0 {
				continue
			}
			select {
			case out <- frame:
			case <-ctx.Done():
				errs <- ctx.Err()
				return
			}
			if frame.Seq == ^uint64(0) {
				return
			}
		}
	}()
	return out, errs
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
	if buf == nil || buf.data == nil || buf.len == 0 {
		return nil
	}
	data := C.GoBytes(unsafe.Pointer(buf.data), C.int(buf.len))
	C.plsm_buf_free(buf)
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
		Seq:         uint64(frame.seq),
		TimestampNs: uint64(frame.timestamp_ns),
		Flags:       uint32(frame.flags),
		Payload:     payload,
	}
	C.plsm_lite3_frame_free(frame)
	return out
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

func bufferSize(input int) int {
	if input <= 0 {
		return 64
	}
	return input
}

func messageHasTags(message *api.Message, required []string) bool {
	if len(required) == 0 {
		return true
	}
	if message == nil {
		return false
	}
	have := make(map[string]struct{}, len(message.Meta.Tags))
	for _, tag := range message.Meta.Tags {
		have[tag] = struct{}{}
	}
	for _, requiredTag := range required {
		if _, ok := have[requiredTag]; !ok {
			return false
		}
	}
	return true
}

func closedError(target string) error {
	return fmt.Errorf("%w: %s is closed", ErrClosed, target)
}

func invalidArgumentError(message string) error {
	return fmt.Errorf("%w: %s", ErrInvalidArgument, message)
}
