/*
Purpose: Backend-agnostic Go contracts and shared model types for Plasmite.
Key Exports: Client, Pool, Stream, TailOptions, ReplayOptions, Error.
Role: Stable pure-Go surface consumed by local/fake/future transport backends.
Invariants: No cgo imports; error kinds are stable; stream APIs remain cancel-safe.
Invariants: Pool ordering and bounded buffering semantics are preserved by implementations.
Notes: Numeric error-kind values match include/plasmite.h for cross-binding parity.
*/
package api

import (
	"context"
	"errors"
	"fmt"
	"time"
)

type ErrorKind int32

const (
	ErrorInternal      ErrorKind = 1
	ErrorUsage         ErrorKind = 2
	ErrorNotFound      ErrorKind = 3
	ErrorAlreadyExists ErrorKind = 4
	ErrorBusy          ErrorKind = 5
	ErrorPermission    ErrorKind = 6
	ErrorCorrupt       ErrorKind = 7
	ErrorIO            ErrorKind = 8
)

type Error struct {
	Kind    ErrorKind
	Message string
	Path    string
	Seq     *uint64
	Offset  *uint64
}

var (
	ErrClosed          = errors.New("plasmite: closed")
	ErrInvalidArgument = errors.New("plasmite: invalid argument")
)

func (e *Error) Error() string {
	if e == nil {
		return "plasmite: <nil error>"
	}
	if e.Path != "" {
		return fmt.Sprintf("plasmite: %s (%s)", e.Message, e.Path)
	}
	return fmt.Sprintf("plasmite: %s", e.Message)
}

func ClosedError(target string) error {
	return fmt.Errorf("%w: %s is closed", ErrClosed, target)
}

func InvalidArgumentError(message string) error {
	return fmt.Errorf("%w: %s", ErrInvalidArgument, message)
}

type Durability uint32

const (
	DurabilityFast  Durability = 0
	DurabilityFlush Durability = 1
)

type PoolRef string

func PoolRefName(name string) PoolRef { return PoolRef(name) }

func PoolRefPath(path string) PoolRef { return PoolRef(path) }

func PoolRefURI(uri string) PoolRef { return PoolRef(uri) }

type Lite3Frame struct {
	Seq         uint64
	TimestampNs uint64
	Flags       uint32
	Payload     []byte
}

type TailOptions struct {
	SinceSeq    *uint64
	MaxMessages *uint64
	Tags        []string
	Timeout     time.Duration
	Buffer      int
}

type ReplayOptions struct {
	Speed       float64
	SinceSeq    *uint64
	MaxMessages *uint64
	Timeout     time.Duration
}

type Client interface {
	Close()
	CreatePool(ref PoolRef, sizeBytes uint64) (Pool, error)
	OpenPool(ref PoolRef) (Pool, error)
}

type Pool interface {
	Close()
	AppendJSON(payload []byte, tags []string, durability Durability) ([]byte, error)
	Append(value any, tags []string, durability Durability) ([]byte, error)
	AppendLite3(payload []byte, durability Durability) (uint64, error)
	GetJSON(seq uint64) ([]byte, error)
	Get(seq uint64) ([]byte, error)
	GetLite3(seq uint64) (*Lite3Frame, error)
	OpenStream(sinceSeq *uint64, maxMessages *uint64, timeoutMs *uint64) (Stream, error)
	OpenLite3Stream(sinceSeq *uint64, maxMessages *uint64, timeoutMs *uint64) (Lite3Stream, error)
	Tail(ctx context.Context, opts TailOptions) (<-chan []byte, <-chan error)
	TailLite3(ctx context.Context, opts TailOptions) (<-chan *Lite3Frame, <-chan error)
	Replay(ctx context.Context, opts ReplayOptions) (<-chan []byte, <-chan error)
}

type Stream interface {
	NextJSON() ([]byte, error)
	Close()
}

type Lite3Stream interface {
	Next() (*Lite3Frame, error)
	Close()
}
