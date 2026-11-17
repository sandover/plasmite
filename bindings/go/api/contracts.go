/*
Purpose: Backend-agnostic Go contracts and shared model types for Plasmite.
Key Exports: Client, Pool, Message, MessageMeta, Reader, Error.
Role: Stable pure-Go surface consumed by local/fake/future transport backends.
Invariants: No cgo imports; error kinds are stable; stream APIs remain cancel-safe.
Invariants: Pool ordering and bounded buffering semantics are preserved by implementations.
Invariants: Message time normalization yields UTC values.
Notes: Numeric error-kind values match include/plasmite.h for cross-binding parity.
*/
package api

import (
	"encoding/json"
	"errors"
	"fmt"
	"path/filepath"
	"strings"
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
const DefaultPoolSize = DefaultPoolSizeBytes

type PoolRef string

func PoolRefName(name string) PoolRef { return PoolRef(name) }

func PoolRefPath(path string) PoolRef {
	if path != "" && !filepath.IsAbs(path) && filepath.VolumeName(path) == "" && !strings.ContainsAny(path, string(filepath.Separator)+"/") {
		return PoolRef("." + string(filepath.Separator) + path)
	}
	return PoolRef(path)
}

func PoolRefURI(uri string) PoolRef { return PoolRef(uri) }

type MessageMeta struct {
	Tags []string `json:"tags"`
}

type Message struct {
	Seq  uint64          `json:"seq"`
	Time uint64          `json:"-"`
	Data json.RawMessage `json:"data"`
	Meta MessageMeta     `json:"meta"`
	Raw  []byte          `json:"-"`
}

// MarshalJSON uses the same UTC nanosecond timestamp as the CLI.
func (m Message) MarshalJSON() ([]byte, error) {
	const nanosecondsPerSecond = 1_000_000_000
	instant := time.Unix(int64(m.Time/nanosecondsPerSecond), int64(m.Time%nanosecondsPerSecond)).UTC()
	type fields Message
	return json.Marshal(struct {
		fields
		Time string `json:"time"`
	}{fields(m), instant.Format("2006-01-02T15:04:05.000000000Z")})
}

func (m *Message) Tags() []string {
	if m == nil {
		return nil
	}
	return m.Meta.Tags
}

func DecodeMessage(raw []byte) (*Message, error) {
	return decodeMessage(raw, false)
}

// DecodeOwnedMessage takes ownership of raw. The caller must not mutate or reuse
// it after this call. Data keeps its own copy, independent of Raw.
func DecodeOwnedMessage(raw []byte) (*Message, error) {
	return decodeMessage(raw, true)
}

func decodeMessage(raw []byte, owned bool) (*Message, error) {
	var payload struct {
		Seq  uint64          `json:"seq"`
		Time string          `json:"time"`
		Data json.RawMessage `json:"data"`
		Meta MessageMeta     `json:"meta"`
	}
	if err := json.Unmarshal(raw, &payload); err != nil {
		return nil, fmt.Errorf("plasmite: decode message: %w", err)
	}
	parsedTime, err := time.Parse(time.RFC3339Nano, payload.Time)
	if err != nil {
		return nil, fmt.Errorf("plasmite: parse message time: %w", err)
	}
	if !owned {
		raw = append([]byte(nil), raw...)
	}
	return &Message{
		Seq:  payload.Seq,
		Time: unixNanoseconds(parsedTime),
		Data: payload.Data,
		Meta: payload.Meta,
		Raw:  raw,
	}, nil
}

type Lite3Frame struct {
	Seq     uint64
	Time    uint64
	Flags   uint32
	Payload []byte
}

// FeedReceipt identifies a successful commit. Time counts Unix nanoseconds.
type FeedReceipt struct {
	Seq  uint64
	Time uint64
}

type Pool interface {
	Close()
	Info() (PoolInfo, error)
	Feed(value any, tags []string, opts ...FeedOption) (*Message, error)
	FeedJSON(payload []byte, tags []string, durability Durability) ([]byte, error)
	Follow(opts ...ReadOption) (Reader, error)
	Fetch(opts ...ReadOption) (Reader, error)
}

type UDPEvent interface {
	udpEvent()
}

type UDPFrame struct {
	Seq     uint64
	Time    uint64
	Payload []byte
	Message *Message
}

func (*UDPFrame) udpEvent() {}

// UDPViewFrame carries a backend's immutable view without tying these contracts
// to cgo. Concrete bindings choose the view type; ordinary messages need no Close.
type UDPViewFrame[V any] struct {
	Seq  uint64
	Time uint64
	View V
}

func (*UDPViewFrame[V]) udpEvent() {}

// UDPGap is the compatibility name for an explicit server skip, never inferred loss.
type UDPGap struct {
	First  uint64
	Last   uint64
	Reason string
}

func (*UDPGap) udpEvent() {}

type UDPProgress struct {
	StartSeq     uint64
	NextSeq      uint64
	EndSeq       *uint64
	Terminal     bool
	RetentionGap bool
}

func (*UDPProgress) udpEvent() {}
