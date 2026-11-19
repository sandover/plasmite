package local

import (
	"bytes"
	"encoding/json"
	"github.com/sandover/plasmite/bindings/go/v2/api"
	"math"
	"reflect"
	"strconv"
	"strings"
	"testing"
)

type inputJSONHook struct{ run func() ([]byte, error) }

func (hook inputJSONHook) MarshalJSON() ([]byte, error) { return hook.run() }

func TestNativeAppendKeepsTypesAndCommittedSnapshot(t *testing.T) {
	pool := newTestPool(t, newTestClient(t), "native-input")
	source := []byte{0, 255, 3}
	calls := 0
	value := map[string]any{
		"integer": int64(math.MaxInt64), "float": float64(1e20), "negative": math.Copysign(0, -1),
		"bytes": source, "empty": []byte{}, "nil": []byte(nil), "text": "a\x00é",
		"hook": inputJSONHook{run: func() ([]byte, error) { calls++; return []byte(`{"value":7}`), nil }},
	}
	view, err := pool.FeedView(value, []string{"tag\x00tail"})
	if err != nil {
		t.Fatal(err)
	}
	if calls != 1 || view.Seq() == 0 || view.Time() == 0 {
		t.Fatalf("receipt/callback %d %d %d", view.Seq(), view.Time(), calls)
	}
	source[0] = 7
	selector, err := Prepare("/integer", "/float", "/negative", "/bytes", "/empty", "/nil", "/hook/value", "/text")
	if err != nil {
		t.Fatal(err)
	}
	defer selector.Close()
	fields, err := selector.Read(view)
	if err != nil {
		t.Fatal(err)
	}
	if number, ok := fields[0].Int64(); !ok || number != math.MaxInt64 {
		t.Fatal("exact integer")
	}
	if number, ok := fields[1].Float64(); !ok || number != 1e20 {
		t.Fatal("native binary64")
	}
	if number, ok := fields[2].Float64(); !ok || !math.Signbit(number) {
		t.Fatal("negative zero")
	}
	if raw, ok := fields[3].Bytes(); !ok || !bytes.Equal(raw, []byte{0, 255, 3}) {
		t.Fatalf("detached native bytes %v %v", raw, ok)
	}
	if empty, ok := fields[4].Bytes(); !ok || empty == nil || len(empty) != 0 {
		t.Fatal("empty bytes export")
	}
	if fields[4].Kind() != KindBytes || fields[5].Kind() != KindNull {
		t.Fatal("nil/empty byte kinds")
	}
	if number, ok := fields[6].Int64(); !ok || number != 7 {
		t.Fatal("parsed hook subtree")
	}
	if text, ok := fields[7].String(); !ok || text != "a\x00é" {
		t.Fatal("string")
	}
	var meta struct{ Tags []string }
	if err = view.Meta().Decode(&meta); err != nil || !reflect.DeepEqual(meta.Tags, []string{"tag\x00tail"}) {
		t.Fatalf("tags %#v %v", meta, err)
	}
	pool.Close()
	if raw, err := view.JSON(); err != nil || !json.Valid(raw) {
		t.Fatalf("retained output %s %v", raw, err)
	}
}
func TestNativeAppendSnapshotsBytesAndHookBuffersAtVisit(t *testing.T) {
	pool := newTestPool(t, newTestClient(t), "visit-input")
	shared := []byte("ab")
	raw := []byte(`{"value":1}`)
	calls := 0
	value := struct {
		First []byte        `json:"first"`
		A     inputJSONHook `json:"a"`
		B     inputJSONHook `json:"b"`
		Later []byte        `json:"later"`
	}{First: shared, A: inputJSONHook{run: func() ([]byte, error) { calls++; return raw, nil }}, B: inputJSONHook{run: func() ([]byte, error) { calls++; shared[0] = 'z'; raw[len(raw)-2] = '2'; return []byte(`true`), nil }}, Later: shared}
	view, err := pool.FeedView(&value, nil)
	if err != nil {
		t.Fatal(err)
	}
	var decoded struct {
		First []byte
		A     struct{ Value int }
		B     bool
		Later []byte
	}
	if err = view.Decode(&decoded); err != nil {
		t.Fatal(err)
	}
	if handled, err := view.Data().decodeStruct(&decoded); !handled || err != nil {
		t.Fatalf("native bytes decode %v %v", handled, err)
	}
	if calls != 2 || string(decoded.First) != "ab" || string(decoded.Later) != "zb" || decoded.A.Value != 1 || !decoded.B {
		t.Fatalf("visit result %#v, calls %d", decoded, calls)
	}
}
func TestNativeAppendPreservesMapSnapshotAndCloseReentrancy(t *testing.T) {
	pool := newTestPool(t, newTestClient(t), "map-input")
	value := map[string]any{"a": []byte("before"), "c": 7}
	value["b"] = inputJSONHook{run: func() ([]byte, error) { value["c"] = 99; return []byte(`true`), nil }}
	view, err := pool.FeedView(value, nil)
	if err != nil {
		t.Fatal(err)
	}
	var decoded struct {
		A []byte
		B bool
		C int
	}
	if err = view.Decode(&decoded); err != nil || decoded.C != 7 || !decoded.B {
		t.Fatalf("map snapshot %#v %v", decoded, err)
	}
	closing := inputJSONHook{run: func() ([]byte, error) { pool.Close(); return []byte(`{}`), nil }}
	if _, err = pool.FeedView(map[string]any{"hook": closing}, nil); !isUsage(err) {
		t.Fatalf("reentrant close %v", err)
	}
}
func TestNativeAppendRejectsBeforePublicationAndForwardsViews(t *testing.T) {
	pool := newTestPool(t, newTestClient(t), "guard-input")
	first, err := pool.FeedView(map[string]any{"value": 7}, nil)
	if err != nil {
		t.Fatal(err)
	}
	for _, value := range []any{map[string]any{"large": uint64(math.MaxUint64)}, map[string]any{"bad": math.Inf(1)}, []int{1}, map[string]any{"bad": make(chan int)}} {
		if _, err = pool.FeedView(value, nil); err == nil {
			t.Fatalf("accepted %#v", value)
		}
	}
	if _, err = pool.FeedJSONView([]byte(`{"value":9223372036854775808}`), nil); err == nil {
		t.Fatal("accepted large raw integer")
	}
	forwarded, err := pool.FeedView(first.Data(), []string{"forward"})
	if err != nil {
		t.Fatal(err)
	}
	if forwarded.Seq() != first.Seq()+1 {
		t.Fatalf("failed write published: %d -> %d", first.Seq(), forwarded.Seq())
	}
	left, _ := first.DataJSON()
	right, _ := forwarded.DataJSON()
	if !bytes.Equal(left, right) {
		t.Fatalf("forward %s %s", left, right)
	}
	large, err := pool.FeedView(map[string]any{"text": strings.Repeat("x", 100_000)}, nil)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = pool.FeedView(large.Data(), nil); err != nil {
		t.Fatal(err)
	}
}

type inputEmbedded struct {
	Label string `json:"label"`
	Count int    `json:"count"`
}
type inputLeft struct {
	Value string `json:"same"`
}
type inputRight struct {
	Value string `json:"same"`
}
type inputKey int

func (key inputKey) MarshalText() ([]byte, error) { return []byte(strconv.Itoa(int(key) + 10)), nil }

type inputPointerHook int

func (value *inputPointerHook) MarshalJSON() ([]byte, error) {
	return []byte(strconv.Itoa(int(*value) + 1)), nil
}

func TestNativeAppendUsesGoFieldAndKeyRules(t *testing.T) {
	pool := newTestPool(t, newTestClient(t), "go-idioms")
	values := []any{
		struct {
			*inputEmbedded
			Extra string `json:"extra"`
		}{&inputEmbedded{"north", 7}, "x"},
		struct {
			*inputEmbedded
			Extra string `json:"extra"`
		}{nil, "x"},
		struct {
			inputLeft
			inputRight
			Name string `json:"name"`
		}{inputLeft{"one"}, inputRight{"two"}, "north"},
		struct {
			A int `json:"B"`
			B int
		}{1, 2},
		struct {
			First  int    `json:"x"`
			Second int    `json:"x"`
			Dash   string `json:"-,omitempty"`
		}{1, 2, "dash"},
		struct {
			Number  int    `json:"number,string"`
			Text    string `json:"text,string"`
			Omitted string `json:"omit,omitempty"`
		}{7, "north", ""},
		map[inputKey]string{1: "one", 2: "two"},
		map[int]string{-1: "negative", 2: "two"},
		map[string]string{"bad\xff\xfe": "value\xff\xfe"},
	}
	hook := inputPointerHook(4)
	values = append(values, &struct {
		Hook inputPointerHook `json:"hook"`
	}{hook}, struct {
		Hook inputPointerHook `json:"hook"`
	}{hook})
	for _, value := range values {
		expected, err := json.Marshal(value)
		if err != nil {
			t.Fatal(err)
		}
		view, err := pool.FeedView(value, nil)
		if err != nil {
			t.Fatalf("%T: %v", value, err)
		}
		actual, err := view.DataJSON()
		if err != nil {
			t.Fatal(err)
		}
		var want, got any
		if err = json.Unmarshal(expected, &want); err != nil {
			t.Fatal(err)
		}
		if err = json.Unmarshal(actual, &got); err != nil {
			t.Fatal(err)
		}
		if !reflect.DeepEqual(want, got) {
			t.Fatalf("%T: got %s want %s", value, actual, expected)
		}
	}
	view, err := pool.FeedView(struct {
		Dash string `json:"-,omitempty"`
	}{"dash"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	var target struct {
		Dash string `json:"-,omitempty"`
	}
	if err = view.Decode(&target); err != nil || target.Dash != "dash" {
		t.Fatalf("dash tag %#v %v", target, err)
	}
}

func nativeInputChain(containers int) map[string]any {
	root := map[string]any{}
	current := root
	for i := 1; i < containers; i++ {
		next := map[string]any{}
		current["child"] = next
		current = next
	}
	current["value"] = 7
	return root
}
func TestNativeAppendChecksAbsoluteDepthAcrossSources(t *testing.T) {
	pool := newTestPool(t, newTestClient(t), "input-depth")
	valid, err := pool.FeedView(nativeInputChain(31), nil)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = pool.FeedView(valid.Data(), nil); err != nil {
		t.Fatalf("root source copy: %v", err)
	}
	for _, value := range []any{nativeInputChain(32), map[string]any{"nested": valid.Data()}, map[string]any{"hook": inputJSONHook{run: func() ([]byte, error) { return json.Marshal(nativeInputChain(31)) }}}} {
		if _, err = pool.FeedView(value, nil); err == nil {
			t.Fatal("accepted excessive absolute depth")
		}
	}
}
func TestNativeFeedOptionsCannotMutatePriorSnapshots(t *testing.T) {
	pool := newTestPool(t, newTestClient(t), "option-input")
	raw := []byte(`{"value":1}`)
	tags := []string{"before"}
	option := func(config *api.FeedConfig) { raw[len(raw)-2] = '2'; tags[0] = "after" }
	view, err := pool.FeedJSONView(raw, tags, option)
	if err != nil {
		t.Fatal(err)
	}
	var data struct{ Value int }
	if err = view.Decode(&data); err != nil || data.Value != 1 {
		t.Fatalf("snapshot %#v %v", data, err)
	}
	var meta struct{ Tags []string }
	if err = view.Meta().Decode(&meta); err != nil || !reflect.DeepEqual(meta.Tags, []string{"after"}) {
		t.Fatalf("option tags %#v %v", meta, err)
	}
}

func TestNativeAppendPackedScalarsPreserveTypesSnapshotsAndHooks(t *testing.T) {
	pool := newTestPool(t, newTestClient(t), "packed-input")
	integers := []int{1, 2, 3}
	reals := []float64{math.Copysign(0, -1), 1e20}
	flags := []bool{true, false}
	hook := inputJSONHook{run: func() ([]byte, error) {
		integers[0] = 99
		reals[0] = 99
		flags[0] = false
		return []byte(`0`), nil
	}}
	view, err := pool.FeedView(struct {
		Integers []int         `json:"integers"`
		Reals    []float64     `json:"reals"`
		Flags    []bool        `json:"flags"`
		Hook     inputJSONHook `json:"hook"`
		Later    []int         `json:"later"`
		Empty    []int         `json:"empty"`
		Nil      []int         `json:"nil"`
		Array    [2]uint16     `json:"array"`
	}{integers, reals, flags, hook, integers, []int{}, nil, [2]uint16{7, 8}}, nil)
	if err != nil {
		t.Fatal(err)
	}
	var out struct {
		Integers []int
		Reals    []float64
		Flags    []bool
		Later    []int
		Empty    []int
		Nil      []int
		Array    []uint16
	}
	if err = view.Decode(&out); err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(out.Integers, []int{1, 2, 3}) || !reflect.DeepEqual(out.Later, []int{99, 2, 3}) || !reflect.DeepEqual(out.Flags, []bool{true, false}) || !math.Signbit(out.Reals[0]) || out.Reals[1] != 1e20 || out.Empty == nil || out.Nil != nil || !reflect.DeepEqual(out.Array, []uint16{7, 8}) {
		t.Fatalf("wrong packed values: %+v", out)
	}
	// Pointer methods on slice elements keep their normal addressability.
	custom := []inputPointerHook{1, 2}
	expected, err := json.Marshal(struct {
		Values []inputPointerHook `json:"values"`
	}{custom})
	if err != nil {
		t.Fatal(err)
	}
	hooked, err := pool.FeedView(struct {
		Values []inputPointerHook `json:"values"`
	}{custom}, nil)
	if err != nil {
		t.Fatal(err)
	}
	actual, err := hooked.DataJSON()
	if err != nil {
		t.Fatal(err)
	}
	var want, got any
	json.Unmarshal(expected, &want)
	json.Unmarshal(actual, &got)
	if !reflect.DeepEqual(want, got) {
		t.Fatalf("hook mismatch got %s want %s", actual, expected)
	}
	for _, bad := range []any{[]float64{math.Inf(1)}, []float32{float32(math.NaN())}, []uint64{math.MaxUint64}} {
		if _, err = pool.FeedView(map[string]any{"bad": bad}, nil); err == nil {
			t.Fatalf("accepted %v", bad)
		}
	}
}

func TestFeedViewRejectsPointerCycleBeforeAppend(t *testing.T) {
	pool := newTestPool(t, newTestClient(t), "cycle")
	var link any
	link = &link
	if _, err := pool.FeedView(map[string]any{"link": link}, nil); !isUsage(err) {
		t.Fatalf("cyclic pointer: %v", err)
	}
	value := 7
	receipt, err := pool.FeedView(map[string]any{"a": &value, "b": &value}, nil)
	if err != nil || receipt.Seq() != 1 {
		t.Fatalf("shared pointer: %v, %v", receipt, err)
	}
}
