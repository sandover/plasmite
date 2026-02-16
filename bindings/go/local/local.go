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
	"fmt"
	"io"
	"runtime"
	"time"
	"unsafe"

	"github.com/sandover/plasmite/bindings/go/plasmite/api"
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
)

type Error = api.Error

var (
	ErrClosed          = api.ErrClosed
	ErrInvalidArgument = api.ErrInvalidArgument
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

type Lite3Stream struct {
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

type ReplayOptions = api.ReplayOptions

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

func (c *Client) Close() {
	if c == nil || c.ptr == nil {
		return
	}
	C.plsm_client_free(c.ptr)
	c.ptr = nil
}

func (c *Client) CreatePool(ref PoolRef, sizeBytes uint64) (api.Pool, error) {
	if c == nil || c.ptr == nil {
		return nil, closedError("client")
	}
	if ref == "" {
		return nil, invalidArgumentError("pool ref is required")
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
	if c == nil || c.ptr == nil {
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

func (p *Pool) Close() {
	if p == nil || p.ptr == nil {
		return
	}
	C.plsm_pool_free(p.ptr)
	p.ptr = nil
}

func (p *Pool) AppendJSON(payload []byte, tags []string, durability Durability) ([]byte, error) {
	if p == nil || p.ptr == nil {
		return nil, closedError("pool")
	}
	if len(payload) == 0 {
		return nil, invalidArgumentError("payload is required")
	}
	cPayload := (*C.uint8_t)(unsafe.Pointer(&payload[0]))
	cLen := C.size_t(len(payload))

	cDescrips, cleanup := cStringArray(tags)
	defer cleanup()

	var cBuf C.plsm_buf_t
	var cErr *C.plsm_error_t
	rc := C.plsm_pool_append_json(
		p.ptr,
		cPayload,
		cLen,
		cDescrips,
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

func (p *Pool) Append(value any, tags []string, durability Durability) ([]byte, error) {
	payload, err := json.Marshal(value)
	if err != nil {
		return nil, fmt.Errorf("plasmite: marshal payload: %w", err)
	}
	return p.AppendJSON(payload, tags, durability)
}

// AppendLite3 appends a pre-encoded Lite3 payload without JSON encoding.
func (p *Pool) AppendLite3(payload []byte, durability Durability) (uint64, error) {
	if p == nil || p.ptr == nil {
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
	if p == nil || p.ptr == nil {
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

func (p *Pool) Get(seq uint64) ([]byte, error) {
	return p.GetJSON(seq)
}

// GetLite3 returns the raw Lite3 payload and metadata for the given sequence.
func (p *Pool) GetLite3(seq uint64) (*Lite3Frame, error) {
	if p == nil || p.ptr == nil {
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
	if p == nil || p.ptr == nil {
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

func (p *Pool) OpenLite3Stream(sinceSeq *uint64, maxMessages *uint64, timeoutMs *uint64) (api.Lite3Stream, error) {
	if p == nil || p.ptr == nil {
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
	rc := C.plsm_lite3_stream_open(
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
	return &Lite3Stream{ptr: cStream}, nil
}

func (s *Stream) NextJSON() ([]byte, error) {
	if s == nil || s.ptr == nil {
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
	if s == nil || s.ptr == nil {
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
	if s == nil || s.ptr == nil {
		return
	}
	C.plsm_stream_free(s.ptr)
	s.ptr = nil
}

func (s *Lite3Stream) Close() {
	if s == nil || s.ptr == nil {
		return
	}
	C.plsm_lite3_stream_free(s.ptr)
	s.ptr = nil
}

// Tail streams JSON messages on a buffered channel.
// Backpressure: when the buffer is full, tailing blocks until the caller drains it.
// Cancellation: the stream is reopened after Timeout to check ctx; set Timeout for responsiveness.
func (p *Pool) Tail(ctx context.Context, opts TailOptions) (<-chan []byte, <-chan error) {
	out := make(chan []byte, bufferSize(opts.Buffer))
	errs := make(chan error, 1)

	go func() {
		defer close(out)
		defer close(errs)

		var delivered uint64
		var since *uint64
		if opts.SinceSeq != nil {
			start := *opts.SinceSeq
			since = &start
		}

		for {
			if opts.MaxMessages != nil && delivered >= *opts.MaxMessages {
				return
			}
			select {
			case <-ctx.Done():
				errs <- ctx.Err()
				return
			default:
			}

			timeoutMs := opts.Timeout
			if timeoutMs <= 0 {
				timeoutMs = time.Second
			}
			timeoutValue := uint64(timeoutMs.Milliseconds())
			var remaining *uint64
			if opts.MaxMessages != nil {
				left := *opts.MaxMessages - delivered
				remaining = &left
			}
			stream, err := p.OpenStream(since, remaining, &timeoutValue)
			if err != nil {
				errs <- err
				return
			}

			for {
				msg, err := stream.NextJSON()
				if err == io.EOF {
					stream.Close()
					break
				}
				if err != nil {
					stream.Close()
					errs <- err
					return
				}
				if seq, err := extractSeq(msg); err == nil {
					next := seq + 1
					since = &next
				}
				if !messageHasTags(msg, opts.Tags) {
					continue
				}
				delivered++
				select {
				case out <- msg:
					if opts.MaxMessages != nil && delivered >= *opts.MaxMessages {
						stream.Close()
						return
					}
				case <-ctx.Done():
					stream.Close()
					errs <- ctx.Err()
					return
				}
			}
		}
	}()

	return out, errs
}

// TailLite3 streams Lite3 frames on a buffered channel.
// Backpressure: when the buffer is full, tailing blocks until the caller drains it.
// Cancellation: the stream is reopened after Timeout to check ctx; set Timeout for responsiveness.
func (p *Pool) TailLite3(ctx context.Context, opts TailOptions) (<-chan *Lite3Frame, <-chan error) {
	out := make(chan *Lite3Frame, bufferSize(opts.Buffer))
	errs := make(chan error, 1)

	go func() {
		defer close(out)
		defer close(errs)

		var delivered uint64
		var since *uint64
		if opts.SinceSeq != nil {
			start := *opts.SinceSeq
			since = &start
		}

		for {
			if opts.MaxMessages != nil && delivered >= *opts.MaxMessages {
				return
			}
			select {
			case <-ctx.Done():
				errs <- ctx.Err()
				return
			default:
			}

			timeoutMs := opts.Timeout
			if timeoutMs <= 0 {
				timeoutMs = time.Second
			}
			timeoutValue := uint64(timeoutMs.Milliseconds())
			var remaining *uint64
			if opts.MaxMessages != nil {
				left := *opts.MaxMessages - delivered
				remaining = &left
			}
			stream, err := p.OpenLite3Stream(since, remaining, &timeoutValue)
			if err != nil {
				errs <- err
				return
			}

			for {
				frame, err := stream.Next()
				if err == io.EOF {
					stream.Close()
					break
				}
				if err != nil {
					stream.Close()
					errs <- err
					return
				}
				delivered++
				if opts.MaxMessages != nil && delivered >= *opts.MaxMessages {
					stream.Close()
					select {
					case out <- frame:
					case <-ctx.Done():
						errs <- ctx.Err()
					}
					return
				}
				select {
				case out <- frame:
					next := frame.Seq + 1
					since = &next
				case <-ctx.Done():
					stream.Close()
					errs <- ctx.Err()
					return
				}
			}
		}
	}()

	return out, errs
}

// Replay collects all messages from the pool, then yields them with inter-message delays
// scaled by the speed multiplier. Unlike Tail, Replay is bounded — it does not follow live writes.
func (p *Pool) Replay(ctx context.Context, opts ReplayOptions) (<-chan []byte, <-chan error) {
	out := make(chan []byte, 64)
	errs := make(chan error, 1)

	go func() {
		defer close(out)
		defer close(errs)

		timeoutMs := opts.Timeout
		if timeoutMs <= 0 {
			timeoutMs = time.Second
		}
		timeoutValue := uint64(timeoutMs.Milliseconds())

		stream, err := p.OpenStream(opts.SinceSeq, opts.MaxMessages, &timeoutValue)
		if err != nil {
			errs <- err
			return
		}

		var messages [][]byte
		for {
			msg, err := stream.NextJSON()
			if err == io.EOF {
				break
			}
			if err != nil {
				stream.Close()
				errs <- err
				return
			}
			messages = append(messages, msg)
		}
		stream.Close()

		if len(messages) == 0 {
			return
		}

		speed := opts.Speed
		if speed <= 0 {
			speed = 1.0
		}

		timestamps := make([]time.Time, len(messages))
		for i, msg := range messages {
			timestamps[i] = extractTime(msg)
		}

		for i, msg := range messages {
			if i > 0 && !timestamps[i-1].IsZero() && !timestamps[i].IsZero() {
				delta := timestamps[i].Sub(timestamps[i-1])
				if delta > 0 {
					delay := time.Duration(float64(delta) / speed)
					select {
					case <-time.After(delay):
					case <-ctx.Done():
						errs <- ctx.Err()
						return
					}
				}
			}

			select {
			case out <- msg:
			case <-ctx.Done():
				errs <- ctx.Err()
				return
			}
		}
	}()

	return out, errs
}

func extractTime(message []byte) time.Time {
	var payload struct {
		Time string `json:"time"`
	}
	if err := json.Unmarshal(message, &payload); err != nil || payload.Time == "" {
		return time.Time{}
	}
	t, err := time.Parse(time.RFC3339Nano, payload.Time)
	if err != nil {
		return time.Time{}
	}
	return t
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

func extractSeq(message []byte) (uint64, error) {
	var payload struct {
		Seq uint64 `json:"seq"`
	}
	if err := json.Unmarshal(message, &payload); err != nil {
		return 0, err
	}
	return payload.Seq, nil
}

func messageHasTags(message []byte, required []string) bool {
	if len(required) == 0 {
		return true
	}
	var payload struct {
		Meta struct {
			Tags []string `json:"tags"`
		} `json:"meta"`
	}
	if err := json.Unmarshal(message, &payload); err != nil {
		return false
	}
	have := make(map[string]struct{}, len(payload.Meta.Tags))
	for _, tag := range payload.Meta.Tags {
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
