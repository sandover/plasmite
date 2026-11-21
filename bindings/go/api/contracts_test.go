package api

import (
	"encoding/json"
	"strings"
	"testing"
	"time"

	"github.com/sandover/plasmite/bindings/go/v2/internal/message"
)

type testEncoding struct{ dataReads int }

func (e *testEncoding) DataJSON() ([]byte, error) {
	e.dataReads++
	return []byte(`{"ok":true}`), nil
}

func (e *testEncoding) MetaJSON() ([]byte, error) {
	return []byte(`{"tags":["alpha","beta"],"sender":"reader"}`), nil
}

func (e *testEncoding) Decode(destination any) error {
	return json.Unmarshal([]byte(`{"ok":true}`), destination)
}

func TestMessageOwnsItsEncodingAndDecodesDataOnDemand(t *testing.T) {
	instant := uint64(time.Date(2026, 2, 18, 2, 0, 0, 123, time.UTC).UnixNano())
	source := &testEncoding{}
	msg := message.New(7, instant, []byte{1, 2, 3}, source)
	if msg.Time() != instant || msg.Seq() != 7 || msg.Sender() != "reader" {
		t.Fatal("wrong message identity or sender")
	}
	tags := msg.Tags()
	if len(tags) != 2 || tags[0] != "alpha" || tags[1] != "beta" {
		t.Fatalf("wrong tags: %v", tags)
	}
	tags[0] = "mutated"
	msg.Raw()[0] = 9
	if msg.Tags()[0] != "alpha" || msg.Raw()[0] != 1 || source.dataReads != 0 {
		t.Fatal("metadata or raw access changed the snapshot or decoded data")
	}
	msg.Data()[0] = 'X'
	if string(msg.Data()) != `{"ok":true}` || source.dataReads != 1 {
		t.Fatal("data was mutated or read twice")
	}
	encoded, err := json.Marshal(msg)
	if err != nil || !strings.Contains(string(encoded), `"time":"2026-02-18T02:00:00.000000123Z"`) {
		t.Fatalf("encode canonical time: %s (%v)", encoded, err)
	}
}

func TestDefaultPoolSizeBytesMatchesCoreDefault(t *testing.T) {
	const want = 1024 * 1024
	if DefaultPoolSizeBytes != want {
		t.Fatalf("expected default pool size %d, got %d", want, DefaultPoolSizeBytes)
	}
}
