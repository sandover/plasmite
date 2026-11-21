package local

import (
	"bytes"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"runtime"
	"strings"
	"testing"
)

var benchmarkMessage *Message

func BenchmarkNativeInput(b *testing.B) {
	fixtures := []string{"small_map", "small_struct", "wide", "long_string", "bytes", "array"}
	if reverse := os.Getenv("PLASMITE_GO_REVERSE") == "1"; reverse {
		for i, j := 0, len(fixtures)-1; i < j; i, j = i+1, j-1 {
			fixtures[i], fixtures[j] = fixtures[j], fixtures[i]
		}
	}
	for _, fixture := range fixtures {
		b.Run(fixture, func(b *testing.B) {
			client, err := NewClient(filepath.Join(b.TempDir(), "pools"))
			if err != nil {
				b.Fatal(err)
			}
			defer client.Close()
			pooled, err := client.CreatePool(PoolRefName("input"), 4*1024*1024)
			if err != nil {
				b.Fatal(err)
			}
			pool := pooled
			defer pool.Close()
			input := map[string]any{"sensor": "north", "power": 7, "real": 0.25}
			switch fixture {
			case "wide":
				for i := 0; i < 200; i++ {
					input[fmt.Sprintf("field_%03d", i)] = i
				}
			case "long_string":
				input["sensor"] = strings.Repeat("x", 64*1024)
			case "bytes":
				input["bytes"] = bytes.Repeat([]byte{0, 1, 127, 255}, 16*1024)
			case "array":
				values := make([]int, 1000)
				for i := range values {
					values[i] = i
				}
				input["samples"] = values
			}
			var value any = input
			if fixture == "small_struct" {
				value = struct {
					Sensor string  `json:"sensor"`
					Power  int     `json:"power"`
					Real   float64 `json:"real"`
				}{"north", 7, 0.25}
			}
			expected, err := json.Marshal(value)
			if err != nil {
				b.Fatal(err)
			}
			var canonical any
			if err = json.Unmarshal(expected, &canonical); err != nil {
				b.Fatal(err)
			}
			source, err := pool.feedView(value, nil)
			if err != nil {
				b.Fatal(err)
			}
			lanes := []string{"append_native_view", "append_message", "append_json_view", "forward_native_view", "forward_json_view", "pipeline_native", "pipeline_json"}
			if os.Getenv("PLASMITE_GO_REVERSE") == "1" {
				for i, j := 0, len(lanes)-1; i < j; i, j = i+1, j-1 {
					lanes[i], lanes[j] = lanes[j], lanes[i]
				}
			}
			for _, lane := range lanes {
				b.Run(lane, func(b *testing.B) {
					b.ReportAllocs()
					before := runtime.NumCgoCall()
					b.ResetTimer()
					for i := 0; i < b.N; i++ {
						var view *messageView
						var err error
						switch lane {
						case "append_message":
							benchmarkMessage, err = pool.Feed(value, nil)
						case "append_native_view", "pipeline_native":
							view, err = pool.feedView(value, nil)
						case "append_json_view", "pipeline_json":
							var raw []byte
							raw, err = json.Marshal(value)
							if err == nil {
								view, err = pool.feedJSONView(raw, nil)
							}
						case "forward_native_view":
							view, err = pool.feedView(source.Data(), nil)
						case "forward_json_view":
							var raw []byte
							raw, err = source.DataJSON()
							if err == nil {
								view, err = pool.feedJSONView(raw, nil)
							}
						}
						if err != nil {
							b.Fatal(err)
						}
						benchmarkView = view
						if lane == "pipeline_native" || lane == "pipeline_json" {
							fetched, e := pool.getView(view.Seq())
							if e != nil {
								b.Fatal(e)
							}
							if fixture == "bytes" {
								var out struct{ Bytes []byte }
								if e = fetched.Decode(&out); e != nil {
									b.Fatal(e)
								}
								benchmarkBytes = out.Bytes
							} else {
								var out struct {
									Sensor string
									Power  int
									Real   float64
								}
								if e = fetched.Decode(&out); e != nil {
									b.Fatal(e)
								}
								benchmarkText = out.Sensor
							}
						}
					}
					b.StopTimer()
					b.ReportMetric(float64(runtime.NumCgoCall()-before)/float64(b.N), "cgo/op")
					var raw []byte
					if lane == "append_message" {
						raw = benchmarkMessage.Data()
					} else {
						raw, err = benchmarkView.DataJSON()
						if err != nil {
							b.Fatal(err)
						}
					}
					var actual any
					if err = json.Unmarshal(raw, &actual); err != nil {
						b.Fatal(err)
					}
					if !reflect.DeepEqual(actual, canonical) {
						b.Fatal("producer result mismatch")
					}
					if strings.HasPrefix(lane, "pipeline") && fixture == "bytes" && !bytes.Equal(benchmarkBytes, input["bytes"].([]byte)) {
						b.Fatal("pipeline bytes mismatch")
					}
				})
			}
		})
	}
}
