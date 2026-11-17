/*
Purpose: Validate API contract helpers and message normalization logic.
Key Exports: None (package tests).
Role: Keep shared contract behavior deterministic without cgo dependencies.
Invariants: Decoded message times are normalized to UTC.
Invariants: Message tags are sourced from Message.Meta.Tags.
Notes: These tests run under `CGO_ENABLED=0` in bindings-go-contract-test.
*/
package api

import (
	"encoding/json"
	"strings"
	"testing"
	"time"
)

func TestDecodeMessageNormalizesUTC(t *testing.T) {
	raw := []byte(`{"seq":7,"time":"2026-02-18T05:00:00+03:00","meta":{"tags":["alpha","beta"]},"data":{"ok":true}}`)
	msg, err := DecodeMessage(raw)
	if err != nil {
		t.Fatalf("decode message: %v", err)
	}
	want := uint64(time.Date(2026, 2, 18, 2, 0, 0, 0, time.UTC).UnixNano())
	if msg.Time != want {
		t.Fatalf("expected %d ns, got %d", want, msg.Time)
	}
	encoded, err := json.Marshal(msg)
	if err != nil || !strings.Contains(string(encoded), `"time":"2026-02-18T02:00:00.000000000Z"`) {
		t.Fatalf("encode canonical time: %s (%v)", encoded, err)
	}
}

func TestMessageTagsAccessorUsesMeta(t *testing.T) {
	msg := &Message{Meta: MessageMeta{Tags: []string{"one", "two"}}}
	tags := msg.Tags()
	if len(tags) != 2 || tags[0] != "one" || tags[1] != "two" {
		t.Fatalf("unexpected tags: %#v", tags)
	}
}

func TestDefaultPoolSizeBytesMatchesCoreDefault(t *testing.T) {
	const want = 1024 * 1024
	if DefaultPoolSizeBytes != want {
		t.Fatalf("expected default pool size %d, got %d", want, DefaultPoolSizeBytes)
	}
}

func TestMessageOwnershipAndDataIsolation(t *testing.T) {
	const wire = `{"seq":1,"time":"2026-01-01T00:00:00Z","meta":{"tags":[]},"data":{"ok":true}}`
	for _, owned := range []bool{false, true} {
		raw := []byte(wire)
		decode := DecodeMessage
		if owned {
			decode = DecodeOwnedMessage
		}
		msg, err := decode(raw)
		if err != nil {
			t.Fatal(err)
		}
		if (&msg.Raw[0] == &raw[0]) != owned {
			t.Fatal("wrong raw ownership")
		}
		// Both public fields remain independently mutable.
		before := string(msg.Data)
		for i := range msg.Raw {
			msg.Raw[i] = ' '
		}
		if string(msg.Data) != before {
			t.Fatal("raw mutation changed data")
		}
		if !owned && string(raw) != wire {
			t.Fatal("defensive decode modified caller bytes")
		}
	}
}

func BenchmarkDecodeOwnership(b *testing.B) {
	wire := []byte(`{"seq":1,"time":"2026-01-01T00:00:00Z","meta":{"tags":[]},"data":{"text":"` + strings.Repeat("x", 1024) + `"}}`)
	for name, decode := range map[string]func([]byte) (*Message, error){"copy": DecodeMessage, "owned": DecodeOwnedMessage} {
		b.Run(name, func(b *testing.B) {
			b.ReportAllocs()
			for i := 0; i < b.N; i++ {
				if _, err := decode(append([]byte(nil), wire...)); err != nil {
					b.Fatal(err)
				}
			}
		})
	}
}
