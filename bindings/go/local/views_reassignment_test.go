package local

import (
	"bytes"
	"encoding/json"
	"reflect"
	"runtime"
	"testing"
)

func TestManagedChildrenKeepTheirSnapshotAfterWrapperReassignment(t *testing.T) {
	pool := newTestPool(t, newTestClient(t), "reassigned-wrapper")
	message, err := pool.FeedView(map[string]any{"object": map[string]any{"power": 7, "label": "north"}, "text": "original text", "blob": []byte{0, 255, 7}, "array": []int{3, 5, 8}}, []string{"original"})
	if err != nil {
		t.Fatal(err)
	}
	pointerAlias := message
	originalAlias := *message
	data := message.Data()
	meta := message.Meta()
	aliasData := originalAlias.Data()
	selector, err := Prepare("/object", "/text", "/blob", "/array", "/object/power")
	if err != nil {
		t.Fatal(err)
	}
	defer selector.Close()
	children, err := selector.Read(message)
	if err != nil {
		t.Fatal(err)
	}
	replacement, err := pool.FeedView(map[string]any{"text": "tiny"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	*pointerAlias = *replacement
	var changed struct{ Text string }
	if err = message.Decode(&changed); err != nil || changed.Text != "tiny" {
		t.Fatalf("wrapper did not change: %+v %v", changed, err)
	}
	var unchanged struct{ Text string }
	if err = originalAlias.Decode(&unchanged); err != nil || unchanged.Text != "original text" {
		t.Fatalf("copied alias changed: %+v %v", unchanged, err)
	}
	*message = MessageView{}
	originalAlias = MessageView{}
	replacement = nil
	pool.Close()
	runtime.GC()
	for _, root := range []Value{data, aliasData} {
		var out struct{ Text string }
		if err = root.Decode(&out); err != nil || out.Text != "original text" {
			t.Fatalf("retained root: %+v %v", out, err)
		}
	}
	var metadata struct{ Tags []string }
	if err = meta.Decode(&metadata); err != nil || !reflect.DeepEqual(metadata.Tags, []string{"original"}) {
		t.Fatalf("retained meta: %+v %v", metadata, err)
	}
	var object struct {
		Power int
		Label string
	}
	if err = children[0].Decode(&object); err != nil || object.Power != 7 || object.Label != "north" {
		t.Fatalf("retained object: %+v %v", object, err)
	}
	if text, ok := children[1].String(); !ok || text != "original text" {
		t.Fatalf("retained scalar: %q %v", text, ok)
	}
	if blob, ok := children[2].Bytes(); !ok || !bytes.Equal(blob, []byte{0, 255, 7}) {
		t.Fatalf("retained bytes: %v %v", blob, ok)
	}
	var array []int
	if err = children[3].Decode(&array); err != nil || !reflect.DeepEqual(array, []int{3, 5, 8}) {
		t.Fatalf("retained array: %v %v", array, err)
	}
	scalar, err := children[4].JSON()
	if err != nil || string(scalar) != "7" {
		t.Fatalf("retained numeric node: %s %v", scalar, err)
	}
	// Source Value forwarding must use its captured bytes, too.
	destination := newTestPool(t, newTestClient(t), "retained-forward")
	forwarded, err := destination.FeedView(children[0], nil)
	if err != nil {
		t.Fatal(err)
	}
	var actual map[string]any
	if err = forwarded.Decode(&actual); err != nil || actual["label"] != "north" {
		t.Fatalf("retained forward: %v %v", actual, err)
	}
}

func TestManagedChildKeepsDuplicateProofAfterWrapperReassignment(t *testing.T) {
	pool := newTestPool(t, newTestClient(t), "reassigned-duplicate")
	original, err := pool.FeedView(map[string]any{"ab": 1, "bA": 2}, nil)
	if err != nil {
		t.Fatal(err)
	}
	encoded := original.Lite3()
	key := bytes.Index(encoded, []byte("bA\x00"))
	if key < 0 {
		t.Fatal("missing collision key")
	}
	copy(encoded[key:key+2], "ab")
	seq, err := feedLite3Sequence(pool, encoded, DurabilityFast)
	if err != nil {
		t.Fatal(err)
	}
	message, err := pool.getView(seq)
	if err != nil {
		t.Fatal(err)
	}
	child := message.Data()
	*message = MessageView{}
	runtime.GC()
	selector, err := Prepare("/ab")
	if err != nil {
		t.Fatal(err)
	}
	defer selector.Close()
	slots, err := selector.read(child.owner, child.node)
	if err != nil || int64(slots[0].integer) != 2 {
		t.Fatalf("lost canonical duplicate proof: %v %v", slots, err)
	}
	data, err := child.JSON()
	if err != nil {
		t.Fatal(err)
	}
	var out map[string]int
	if err = json.Unmarshal(data, &out); err != nil || !reflect.DeepEqual(out, map[string]int{"ab": 2}) {
		t.Fatalf("retained duplicate JSON: %s %v", data, err)
	}
}
