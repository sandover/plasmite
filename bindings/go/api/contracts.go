/*
Purpose: Backend-agnostic Go contracts and shared model types for Plasmite.
Key Exports: Pool, Message, Reader, Error.
Role: Stable pure-Go surface consumed by local/fake/future transport backends.
Invariants: No cgo imports; error kinds are stable; stream APIs remain cancel-safe.
Invariants: Pool ordering and bounded buffering semantics are preserved by implementations.
Invariants: Message time normalization yields UTC values.
Notes: Numeric error-kind values match include/plasmite.h for cross-binding parity.
*/
package api

import (
	"encoding/json"
	"fmt"
	"path/filepath"
	"strings"

	"github.com/sandover/plasmite/bindings/go/v2/internal/message"
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
	ErrorRetentionGap  ErrorKind = 9
)

type Error struct {
	Kind    ErrorKind
	Message string
	Path    string
	Seq     *uint64
	Offset  *uint64
	Missing uint64
	NextSeq uint64
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

func ClosedError(target string) error {
	return &Error{Kind: ErrorUsage, Message: target + " is closed"}
}

func InvalidArgumentError(message string) error {
	return &Error{Kind: ErrorUsage, Message: message}
}

type Durability uint32

const (
	DurabilityFast  Durability = 0
	DurabilityFlush Durability = 1
)

type FeedConfig struct {
	Durability Durability
}

type FeedOption func(*FeedConfig)

func WithDurability(d Durability) FeedOption {
	return func(cfg *FeedConfig) {
		cfg.Durability = d
	}
}

func ApplyFeedOptions(opts ...FeedOption) FeedConfig {
	cfg := FeedConfig{Durability: DurabilityFast}
	for _, opt := range opts {
		if opt != nil {
			opt(&cfg)
		}
	}
	return cfg
}

const DefaultPoolSizeBytes uint64 = 1024 * 1024

type PoolRef string

func PoolRefName(name string) PoolRef { return PoolRef(name) }

func PoolRefPath(path string) PoolRef {
	if path != "" && !filepath.IsAbs(path) && filepath.VolumeName(path) == "" && !strings.ContainsAny(path, string(filepath.Separator)+"/") {
		return PoolRef("." + string(filepath.Separator) + path)
	}
	return PoolRef(path)
}

func PoolRefURI(uri string) PoolRef { return PoolRef(uri) }

// Message is an immutable record returned by Feed, Follow and Fetch.
type Message = message.Message

// JSON passes an explicitly encoded data object to Feed.
type JSON = json.RawMessage

// Lite3 passes the stored encoding of a message to Feed.
type Lite3 []byte

type Pool interface {
	Close()
	Info() (PoolInfo, error)
	Feed(value any, tags []string, opts ...FeedOption) (*Message, error)
	Follow(opts ...ReadOption) (Reader, error)
	Fetch(opts ...ReadOption) (Reader, error)
}

type UDPEvent interface {
	udpEvent()
}

type UDPFrame struct{ Message *Message }

func (*UDPFrame) udpEvent() {}

// UDPSkip reports a range the server explicitly skipped.
type UDPSkip struct {
	First  uint64
	Last   uint64
	Reason string
}

func (*UDPSkip) udpEvent() {}

type UDPProgress struct {
	StartSeq uint64
	NextSeq  uint64
	EndSeq   *uint64
	Terminal bool
}

func (*UDPProgress) udpEvent() {}
