package local

import (
	"encoding/json"
	"errors"
	"reflect"
	"strings"
	"sync"
	"testing"
)

func TestManagedViewRetainsSnapshotAndCopiesExports(t *testing.T) {
	client := newTestClient(t)
	pool := newTestPool(t, client, "views").(*Pool)
	original, err := pool.AppendJSON([]byte(`{"sensor":"north","metrics":{"power":7},"large":9223372036854775807,"zero":-0.0,"values":[1,null,"é"]}`), []string{"test"}, DurabilityFast)
	if err != nil {
		t.Fatal(err)
	}
	var receipt struct{ Seq uint64 }
	if err = json.Unmarshal(original, &receipt); err != nil {
		t.Fatal(err)
	}
	view, err := pool.GetView(receipt.Seq)
	if err != nil {
		t.Fatal(err)
	}
	selector, err := Prepare("/sensor", "/metrics/power", "/large", "/values/2", "/missing")
	if err != nil {
		t.Fatal(err)
	}
	defer selector.Close()
	values, err := selector.Read(view)
	if err != nil {
		t.Fatal(err)
	}
	if got, ok := values[0].String(); !ok || got != "north" {
		t.Fatalf("sensor %q %v", got, ok)
	}
	if got, ok := values[1].Int64(); !ok || got != 7 {
		t.Fatalf("power %d %v", got, ok)
	}
	if got, ok := values[2].Int64(); !ok || got != 9223372036854775807 {
		t.Fatalf("large %d %v", got, ok)
	}
	if got, ok := values[3].String(); !ok || got != "é" {
		t.Fatalf("Unicode %q %v", got, ok)
	}
	if values[4].Kind() != 0 {
		t.Fatal("missing value present")
	}
	detached := view.Lite3()
	for i := range detached {
		detached[i] = 0
	}
	pool.Close()
	if _, err = pool.GetView(receipt.Seq); !errors.Is(err, ErrClosed) {
		t.Fatalf("closed %v", err)
	}
	var decoded struct {
		Sensor  string
		Metrics struct{ Power int }
		Large   int64
		Values  []any
	}
	if err = view.Decode(&decoded); err != nil {
		t.Fatal(err)
	}
	if decoded.Sensor != "north" || decoded.Metrics.Power != 7 || decoded.Large != 9223372036854775807 {
		t.Fatalf("decoded %#v", decoded)
	}
	raw, err := view.JSON()
	if err != nil {
		t.Fatal(err)
	}
	var envelope struct {
		Seq  uint64
		Time string
		Meta struct{ Tags []string }
		Data map[string]json.RawMessage
	}
	if err = json.Unmarshal(raw, &envelope); err != nil {
		t.Fatalf("%s: %v", raw, err)
	}
	if envelope.Seq != view.Seq() || len(envelope.Meta.Tags) != 1 || envelope.Meta.Tags[0] != "test" || !strings.HasSuffix(envelope.Time, "Z") {
		t.Fatalf("envelope %#v", envelope)
	}
	if string(envelope.Data["zero"]) != "-0.0" {
		t.Fatalf("negative zero %s", envelope.Data["zero"])
	}
	raw[0] = 'x'
	if _, err = view.JSON(); err != nil {
		t.Fatal(err)
	}
	var sensor string
	if err = values[0].Decode(&sensor); err != nil || sensor != "north" {
		t.Fatalf("retained child %q %v", sensor, err)
	}
}

func TestManagedViewRetriesCapacityAndAllowsConcurrentReads(t *testing.T) {
	pool := newTestPool(t, newTestClient(t), "large-views").(*Pool)
	text := strings.Repeat("é", 40_000)
	receipt, err := pool.AppendIdentity(map[string]any{"text": text, "power": 7}, nil)
	if err != nil {
		t.Fatal(err)
	}
	view, err := pool.GetView(receipt.Seq)
	if err != nil {
		t.Fatal(err)
	}
	selector, err := Prepare("/text", "/power")
	if err != nil {
		t.Fatal(err)
	}
	defer selector.Close()
	var group sync.WaitGroup
	for i := 0; i < 8; i++ {
		group.Add(1)
		go func() {
			defer group.Done()
			for j := 0; j < 20; j++ {
				values, err := selector.Read(view)
				if err != nil {
					t.Error(err)
					return
				}
				if got, ok := values[0].String(); !ok || got != text {
					t.Error("text mismatch")
					return
				}
			}
		}()
	}
	group.Wait()
	selector.Close()
	if _, err = selector.Read(view); !errors.Is(err, ErrClosed) {
		t.Fatalf("closed selector %v", err)
	}
}

type typedReading struct {
	Sensor  string `json:"sensor"`
	Metrics struct {
		Power int64 `json:"power"`
	} `json:"metrics"`
	Zero float64 `json:"zero"`
}

func TestManagedViewTypedDecodeMatchesStandardJSON(t *testing.T) {
	pool := newTestPool(t, newTestClient(t), "typed-views").(*Pool)
	for _, text := range []string{
		`{"sensor":"north","metrics":{"power":9223372036854775807},"zero":-0.0}`,
		`{"sensor":"north","SENSOR":"later","metrics":{"power":7},"zero":1}`,
		`{"sensor":"north","metrics":{"power":7},"zero":1,"extra":true}`,
		`{"sensor":"north","metrics":{},"zero":1}`,
		`{"sensor":null,"metrics":{"power":null},"zero":1}`,
		`{"sensor":"north","metrics":{"power":1.5},"zero":1}`,
		`{"sensor":"north","metrics":{"power":7},"zero":"bad"}`,
	} {
		receipt, err := pool.AppendJSONIdentity([]byte(text), nil, DurabilityFast)
		if err != nil {
			t.Fatal(err)
		}
		view, err := pool.GetView(receipt.Seq)
		if err != nil {
			t.Fatal(err)
		}
		raw, err := view.DataJSON()
		if err != nil {
			t.Fatal(err)
		}
		want, got := typedReading{Sensor: "keep"}, typedReading{Sensor: "keep"}
		wantErr := json.Unmarshal(raw, &want)
		gotErr := view.Decode(&got)
		if (wantErr == nil) != (gotErr == nil) || !reflect.DeepEqual(want, got) {
			t.Fatalf("%s: got %#v/%v want %#v/%v", raw, got, gotErr, want, wantErr)
		}
	}
	if planFor(reflect.TypeOf(typedReading{})) == nil {
		t.Fatal("ordinary nested struct has no direct plan")
	}
}

func TestManagedViewDuplicateProofCrossesBoundaryWithoutPointers(t *testing.T) {
	pool := newTestPool(t, newTestClient(t), "duplicate-views").(*Pool)
	receipt, err := pool.AppendIdentity(map[string]any{"ab": 1, "bA": 2}, nil)
	if err != nil {
		t.Fatal(err)
	}
	original, err := pool.GetView(receipt.Seq)
	if err != nil {
		t.Fatal(err)
	}
	both, err := Prepare("/ab", "/bA")
	if err != nil {
		t.Fatal(err)
	}
	defer both.Close()
	values, err := both.Read(original)
	if err != nil {
		t.Fatal(err)
	}
	bytes := original.Lite3()
	second := int(values[1].node)
	copy(bytes[second-3:second-1], "ab") // Valid collision probe: ab and bA share their native hash.
	seq, err := pool.AppendLite3(bytes, DurabilityFast)
	if err != nil {
		t.Fatal(err)
	}
	view, err := pool.GetView(seq)
	if err != nil {
		t.Fatal(err)
	}
	last, err := Prepare("/ab")
	if err != nil {
		t.Fatal(err)
	}
	defer last.Close()
	selected, err := last.Read(view)
	if err != nil {
		t.Fatal(err)
	}
	if n, ok := selected[0].Int64(); !ok || n != 2 {
		t.Fatalf("duplicate %d %v", n, ok)
	}
	json, err := view.DataJSON()
	if err != nil || string(json) != `{"ab":2}` {
		t.Fatalf("canonical %s %v", json, err)
	}
}
