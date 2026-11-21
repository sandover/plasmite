// Package message holds immutable messages shared by the Go transports.
package message

import (
	"encoding/json"
	"errors"
	"slices"
	"sync"
	"time"
)

// Encoding reads checked Lite3 bytes without changing their backing storage.
type Encoding interface {
	DataJSON() ([]byte, error)
	MetaJSON() ([]byte, error)
	Decode(any) error
}

type metadata struct {
	Tags   []string `json:"tags"`
	Sender string   `json:"sender,omitempty"`
}

type record struct {
	seq, time uint64
	raw       []byte
	encoding  Encoding

	dataOnce sync.Once
	data     json.RawMessage
	dataErr  error
	metaOnce sync.Once
	meta     metadata
	metaErr  error
}

// Message owns an immutable record and remains valid after its reader or pool
// closes. Copies of a Message share the record and its decoded data cache.
type Message struct{ record *record }

func New(seq, time uint64, raw []byte, encoding Encoding) *Message {
	return &Message{record: &record{seq: seq, time: time, raw: raw, encoding: encoding}}
}

func (m *Message) Seq() uint64  { return m.record.seq }
func (m *Message) Time() uint64 { return m.record.time }

// Raw returns a mutable copy of the stored Lite3 bytes.
func (m *Message) Raw() []byte { return slices.Clone(Bytes(m)) }

// Bytes gives binding code a read-only borrow for forwarding.
func Bytes(m *Message) []byte {
	if m == nil || m.record == nil {
		return nil
	}
	return m.record.raw
}

func StoredEncoding(m *Message) Encoding {
	if m == nil || m.record == nil {
		return nil
	}
	return m.record.encoding
}

// Data returns the JSON object. The message formats the object once, on demand,
// and gives each caller a copy it can modify.
func (m *Message) Data() json.RawMessage {
	data, _ := m.record.dataJSON()
	return slices.Clone(data)
}

// Decode reads the data into a Go value through encoding/json-compatible rules.
func (m *Message) Decode(destination any) error {
	return m.record.encoding.Decode(destination)
}

func (m *Message) Tags() []string { return slices.Clone(m.record.metadata().Tags) }

// Sender is the name of the server access key that wrote this message, or empty
// for a write without a key.
func (m *Message) Sender() string { return m.record.metadata().Sender }

func (r *record) dataJSON() (json.RawMessage, error) {
	r.dataOnce.Do(func() { r.data, r.dataErr = r.encoding.DataJSON() })
	return r.data, r.dataErr
}

func (r *record) metadata() metadata {
	r.metaOnce.Do(func() {
		var raw []byte
		raw, r.metaErr = r.encoding.MetaJSON()
		if r.metaErr == nil {
			r.metaErr = json.Unmarshal(raw, &r.meta)
		}
	})
	return r.meta
}

// MarshalJSON writes the standard message envelope, with a UTC timestamp.
func (m Message) MarshalJSON() ([]byte, error) {
	r := m.record
	if r == nil {
		return nil, errors.New("plasmite: empty message")
	}
	data, err := r.dataJSON()
	if err != nil {
		return nil, err
	}
	meta := r.metadata()
	if r.metaErr != nil {
		return nil, r.metaErr
	}
	const nsPerSecond = 1_000_000_000
	instant := time.Unix(int64(r.time/nsPerSecond), int64(r.time%nsPerSecond)).UTC()
	return json.Marshal(struct {
		Seq  uint64          `json:"seq"`
		Time string          `json:"time"`
		Meta metadata        `json:"meta"`
		Data json.RawMessage `json:"data"`
	}{r.seq, instant.Format("2006-01-02T15:04:05.000000000Z"), meta, data})
}
