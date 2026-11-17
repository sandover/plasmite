package local

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"runtime"
	"strings"
	"testing"

	"github.com/sandover/plasmite/bindings/go/api"
)

var streamBenchmarkValue int
var streamBenchmarkView *MessageView
var streamBenchmarkFull streamDecoded
var streamBenchmarkRaw []byte

// Prefilled pull streams isolate per-record delivery/conversion. Reopening after
// 128 records keeps source size fixed and includes amortized stream setup.
func BenchmarkOwnedStreams(b *testing.B) {
	fixtures := []string{"small", "wide", "long", "array", "bytes"}
	lanes := []string{"raw", "view_selected", "view_full", "json_selected", "json_full", "raw_forward", "view_forward", "view_copy", "json_forward"}
	if os.Getenv("PLASMITE_GO_REVERSE") == "1" {
		reverseStrings(fixtures)
		reverseStrings(lanes)
	}
	for _, fixture := range fixtures {
		b.Run(fixture, func(b *testing.B) {
			client, err := NewClient(filepath.Join(b.TempDir(), "pools"))
			if err != nil {
				b.Fatal(err)
			}
			defer client.Close()
			initial, err := client.CreatePool(PoolRefName("seed"), 1<<20)
			if err != nil {
				b.Fatal(err)
			}
			seed := initial.(*Pool)
			defer seed.Close()
			input := streamFixture(fixture)
			source, err := seed.AppendView(input, nil)
			if err != nil {
				b.Fatal(err)
			}
			encoded := source.Lite3()
			expected, err := source.DataJSON()
			if err != nil {
				b.Fatal(err)
			}
			const window = 128
			for _, lane := range lanes {
				b.Run(lane, func(b *testing.B) {
					laneClient, e := NewClient(filepath.Join(b.TempDir(), "pools"))
					if e != nil {
						b.Fatal(e)
					}
					defer laneClient.Close()
					capacity := uint64((len(encoded)+128)*window*4 + 65536)
					sourceAPI, err := laneClient.CreatePool(PoolRefName("source-"+lane), capacity)
					if err != nil {
						b.Fatal(err)
					}
					pool := sourceAPI.(*Pool)
					defer pool.Close()
					destinationAPI, err := laneClient.CreatePool(PoolRefName("destination-"+lane), capacity)
					if err != nil {
						b.Fatal(err)
					}
					destination := destinationAPI.(*Pool)
					defer destination.Close()
					for i := 0; i < window; i++ {
						if _, err = pool.AppendView(source, nil); err != nil {
							b.Fatal(err)
						}
					}
					var viewStream *ViewStream
					var rawStream api.Lite3Stream
					var jsonStream api.Stream
					open := func() {
						since, max := uint64(1), uint64(window)
						if strings.HasPrefix(lane, "view") {
							viewStream, err = pool.OpenViewFollow(FollowOptions{Start: Sequence(since), MaxMessages: &max})
						} else if strings.HasPrefix(lane, "raw") {
							rawStream, err = pool.OpenLite3Follow(Sequence(since), &max, nil)
						} else {
							jsonStream, err = pool.OpenFollow(Sequence(since), &max, nil)
						}
						if err != nil {
							b.Fatal(err)
						}
					}
					closeReader := func() {
						if viewStream != nil {
							viewStream.Close()
						}
						if rawStream != nil {
							rawStream.Close()
						}
						if jsonStream != nil {
							jsonStream.Close()
						}
					}
					open()
					defer closeReader()
					b.ReportAllocs()
					calls := runtime.NumCgoCall()
					b.ResetTimer()
					for i := 0; i < b.N; i++ {
						if i > 0 && i%window == 0 {
							closeReader()
							open()
						}
						if viewStream != nil {
							view, e := viewStream.Next(context.Background())
							if e != nil {
								b.Fatal(e)
							}
							if view.Seq() != uint64(i%window+1) {
								b.Fatal("out of order")
							}
							switch lane {
							case "view_selected":
								var out struct{ Value int }
								if e = view.Decode(&out); e != nil {
									b.Fatal(e)
								}
								streamBenchmarkValue = out.Value
							case "view_full":
								var out streamDecoded
								if e = view.Decode(&out); e != nil {
									b.Fatal(e)
								}
								streamBenchmarkValue = out.Value
								streamBenchmarkFull = out
							case "view_forward":
								streamBenchmarkView, e = destination.AppendView(view, nil)
							case "view_copy":
								streamBenchmarkView, e = destination.AppendView(view.Data(), nil)
							}
							if e != nil {
								b.Fatal(e)
							}
						} else if rawStream != nil {
							frame, e := rawStream.Next()
							if e != nil {
								b.Fatal(e)
							}
							if frame.Seq != uint64(i%window+1) {
								b.Fatal("out of order")
							}
							if lane == "raw_forward" {
								_, e = destination.AppendLite3(frame.Payload, DurabilityFast)
								if e != nil {
									b.Fatal(e)
								}
							}
							streamBenchmarkValue = len(frame.Payload)
							streamBenchmarkRaw = frame.Payload
						} else {
							raw, e := jsonStream.NextJSON()
							if e != nil {
								b.Fatal(e)
							}
							message, e := api.DecodeOwnedMessage(raw)
							if e != nil {
								b.Fatal(e)
							}
							if message.Seq != uint64(i%window+1) {
								b.Fatal("out of order")
							}
							switch lane {
							case "json_selected":
								var out struct{ Value int }
								e = json.Unmarshal(message.Data, &out)
								streamBenchmarkValue = out.Value
							case "json_full":
								var out streamDecoded
								e = json.Unmarshal(message.Data, &out)
								streamBenchmarkValue = out.Value
								streamBenchmarkFull = out
							case "json_forward":
								streamBenchmarkView, e = destination.AppendJSONView(message.Data, nil)
							}
							if e != nil {
								b.Fatal(e)
							}
						}
					}
					b.StopTimer()
					b.ReportMetric(float64(runtime.NumCgoCall()-calls)/float64(b.N), "cgo/op")
					if strings.HasSuffix(lane, "full") {
						var want streamDecoded
						if e := json.Unmarshal(expected, &want); e != nil {
							b.Fatal(e)
						}
						if !reflect.DeepEqual(want, streamBenchmarkFull) {
							b.Fatal("full conversion mismatch")
						}
					}
					if lane == "raw" && !reflect.DeepEqual(streamBenchmarkRaw, encoded) {
						b.Fatal("raw bytes changed")
					}
					if strings.HasSuffix(lane, "forward") || lane == "view_copy" {
						final, e := destination.GetView(uint64(b.N))
						if e != nil {
							b.Fatal(e)
						}
						actual, e := final.DataJSON()
						if e != nil {
							b.Fatal(e)
						}
						var want, got any
						json.Unmarshal(expected, &want)
						json.Unmarshal(actual, &got)
						if !reflect.DeepEqual(want, got) {
							b.Fatal("forwarded value mismatch")
						}
						if (lane == "raw_forward" || lane == "view_forward") && !reflect.DeepEqual(final.Lite3(), encoded) {
							b.Fatal("forwarded payload changed")
						}
					} else if lane != "raw" && streamBenchmarkValue != 7 {
						b.Fatal("wrong selected value")
					}
				})
			}
		})
	}
}

type streamDecoded struct {
	Value int
	Extra any
	Blob  []byte
}

func streamFixture(fixture string) map[string]any {
	input := map[string]any{"value": 7, "extra": "sensor"}
	switch fixture {
	case "wide":
		fields := map[string]any{}
		for i := 0; i < 200; i++ {
			fields[fmt.Sprintf("field_%03d", i)] = i
		}
		input["extra"] = fields
	case "long":
		input["extra"] = strings.Repeat("x", 32000)
	case "array":
		values := make([]int, 1000)
		for i := range values {
			values[i] = i
		}
		input["extra"] = values
	case "bytes":
		input["blob"] = []byte(strings.Repeat("x", 32000))
	}
	return input
}
func reverseStrings(values []string) {
	for i, j := 0, len(values)-1; i < j; i, j = i+1, j-1 {
		values[i], values[j] = values[j], values[i]
	}
}
