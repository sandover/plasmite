package local

import (
	"encoding/json"
	"math"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

var benchmarkText string
var benchmarkView *MessageView
var benchmarkReading typedReading
var benchmarkValues []Value
var benchmarkBytes []byte
var benchmarkNative map[string]any

func BenchmarkOwnedViews(b *testing.B) {
	fixtures := []string{"small", "wide", "long_string", "array"}
	reverse := os.Getenv("PLASMITE_GO_REVERSE") == "1"
	if reverse {
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
			pooled, err := client.CreatePool(PoolRefName("bench"), 4*1024*1024)
			if err != nil {
				b.Fatal(err)
			}
			pool := pooled.(*Pool)
			defer pool.Close()
			sensor := "north"
			if fixture == "long_string" {
				sensor = strings.Repeat("x", 64*1024)
			}
			input := map[string]any{"sensor": sensor, "metrics": map[string]any{"power": int64(7)}, "zero": math.Copysign(0, -1)}
			if fixture == "wide" {
				for i := 0; i < 200; i++ {
					input[strings.Repeat("x", i%10)+string(rune(0x100+i))] = i
				}
			}
			if fixture == "array" {
				values := make([]int, 1000)
				for i := range values {
					values[i] = i
				}
				input["samples"] = values
			}
			receipt, err := pool.AppendIdentity(input, nil)
			if err != nil {
				b.Fatal(err)
			}
			view, err := pool.GetView(receipt.Seq)
			if err != nil {
				b.Fatal(err)
			}
			raw, err := view.DataJSON()
			if err != nil {
				b.Fatal(err)
			}
			var native map[string]any
			if err = json.Unmarshal(raw, &native); err != nil {
				b.Fatal(err)
			}
			selector, err := Prepare("/sensor", "/metrics/power")
			if err != nil {
				b.Fatal(err)
			}
			defer selector.Close()
			// Prepare the type plan before warm and cold measurements alike.
			var warm typedReading
			if err = view.Decode(&warm); err != nil {
				b.Fatal(err)
			}
			lanes := []string{"get_into", "get_into_exact", "get_c_copy", "cold_view_typed", "cold_json_typed", "cold_view_selected", "cold_json_selected", "warm_view_typed", "warm_json_typed", "warm_view_selected", "warm_native_selected", "full_view_native", "full_json_native"}
			if reverse {
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
						switch lane {
						case "get_into":
							v, e := pool.GetView(receipt.Seq)
							if e != nil {
								b.Fatal(e)
							}
							benchmarkView = v
						case "get_into_exact":
							v, e := pool.copyViewCapacity(receipt.Seq, 0)
							if e != nil {
								b.Fatal(e)
							}
							if e = v.validate(); e != nil {
								b.Fatal(e)
							}
							benchmarkView = v
						case "get_c_copy":
							frame, e := pool.GetLite3(receipt.Seq)
							if e != nil {
								b.Fatal(e)
							}
							v := &MessageView{encoded: frame.Payload, seq: frame.Seq, timestampNs: frame.TimestampNs}
							if e = v.validate(); e != nil {
								b.Fatal(e)
							}
							benchmarkView = v
						case "cold_view_typed":
							v, e := pool.GetView(receipt.Seq)
							if e != nil {
								b.Fatal(e)
							}
							var out typedReading
							if e = v.Decode(&out); e != nil {
								b.Fatal(e)
							}
							benchmarkReading = out
						case "cold_json_typed", "cold_json_selected":
							data, e := pool.GetJSON(receipt.Seq)
							if e != nil {
								b.Fatal(e)
							}
							var out struct{ Data typedReading }
							if e = json.Unmarshal(data, &out); e != nil {
								b.Fatal(e)
							}
							benchmarkReading = out.Data
						case "cold_view_selected":
							v, e := pool.GetView(receipt.Seq)
							if e != nil {
								b.Fatal(e)
							}
							values, e := selector.Read(v)
							if e != nil {
								b.Fatal(e)
							}
							text, ok := values[0].String()
							number, numberOK := values[1].Int64()
							if !ok || !numberOK || number != 7 || text != sensor {
								b.Fatal("projection mismatch")
							}
							benchmarkValues = values
							benchmarkText = text
						case "warm_view_typed":
							var out typedReading
							if e := view.Decode(&out); e != nil {
								b.Fatal(e)
							}
							benchmarkReading = out
						case "warm_json_typed":
							var out typedReading
							if e := json.Unmarshal(raw, &out); e != nil {
								b.Fatal(e)
							}
							benchmarkReading = out
						case "warm_view_selected":
							values, e := selector.Read(view)
							if e != nil {
								b.Fatal(e)
							}
							text, ok := values[0].String()
							number, numberOK := values[1].Int64()
							if !ok || !numberOK || number != 7 || text != sensor {
								b.Fatal("projection mismatch")
							}
							benchmarkValues = values
							benchmarkText = text
						case "warm_native_selected":
							if native["sensor"].(string) != sensor || native["metrics"].(map[string]any)["power"].(float64) != 7 {
								b.Fatal("native mismatch")
							}
							benchmarkNative = native
						case "full_view_native":
							var out map[string]any
							if e := view.Decode(&out); e != nil {
								b.Fatal(e)
							}
							benchmarkNative = out
						case "full_json_native":
							var out map[string]any
							if e := json.Unmarshal(raw, &out); e != nil {
								b.Fatal(e)
							}
							benchmarkNative = out
						}
					}
					b.StopTimer()
					if lane == "full_view_native" || lane == "full_json_native" {
						if benchmarkNative["sensor"] != sensor || benchmarkNative["metrics"].(map[string]any)["power"].(float64) != 7 {
							b.Fatal("full native mismatch")
						}
						if fixture == "array" {
							sum := 0
							for _, value := range benchmarkNative["samples"].([]any) {
								sum += int(value.(float64))
							}
							if sum != 499500 {
								b.Fatal("array checksum mismatch")
							}
						}
					}

					b.ReportMetric(float64(runtime.NumCgoCall()-before)/float64(b.N), "cgo/op")
					if strings.Contains(lane, "typed") || lane == "cold_json_selected" {
						if benchmarkReading.Sensor != sensor || benchmarkReading.Metrics.Power != 7 {
							b.Fatal("typed mismatch")
						}
					}
				})
			}
		})
	}
}
