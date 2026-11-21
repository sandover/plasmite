//go:build go1.24

package local

import (
	"encoding/json"
	"reflect"
	"testing"
)

type inputZeroValue struct{ Value int }

func (value inputZeroValue) IsZero() bool { return value.Value == 7 }
func TestNativeAppendOmitZeroMatchesCurrentGo(t *testing.T) {
	pool := newTestPool(t, newTestClient(t), "zero-input")
	value := struct {
		A inputZeroValue `json:"a,omitzero"`
		B int            `json:"b,omitzero"`
		C string         `json:"c"`
	}{inputZeroValue{7}, 0, "keep"}
	expected, err := json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	view, err := pool.feedView(value, nil)
	if err != nil {
		t.Fatal(err)
	}
	actual, err := view.DataJSON()
	if err != nil {
		t.Fatal(err)
	}
	var want, got any
	json.Unmarshal(expected, &want)
	json.Unmarshal(actual, &got)
	if !reflect.DeepEqual(want, got) {
		t.Fatalf("got %s want %s", actual, expected)
	}
}
