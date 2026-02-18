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
	"testing"
	"time"
)

func TestDecodeMessageNormalizesUTC(t *testing.T) {
	raw := []byte(`{"seq":7,"time":"2026-02-18T05:00:00+03:00","meta":{"tags":["alpha","beta"]},"data":{"ok":true}}`)
	msg, err := DecodeMessage(raw)
	if err != nil {
		t.Fatalf("decode message: %v", err)
	}
	if got := msg.Time.Location(); got != time.UTC {
		t.Fatalf("expected UTC location, got %v", got)
	}
	if msg.Time.Hour() != 2 {
		t.Fatalf("expected UTC hour 2, got %d", msg.Time.Hour())
	}
	if msg.TimeRFC3339 != "2026-02-18T05:00:00+03:00" {
		t.Fatalf("unexpected time string: %s", msg.TimeRFC3339)
	}
}

func TestMessageTagsAccessorUsesMeta(t *testing.T) {
	msg := &Message{Meta: MessageMeta{Tags: []string{"one", "two"}}}
	tags := msg.Tags()
	if len(tags) != 2 || tags[0] != "one" || tags[1] != "two" {
		t.Fatalf("unexpected tags: %#v", tags)
	}
}
