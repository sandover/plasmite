package udp

import (
	"bytes"
	"context"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"reflect"
	"runtime"
	"strings"
	"testing"
	"time"

	"github.com/sandover/plasmite/bindings/go/local"
)

var udpBenchmarkValue int
var udpBenchmarkView *local.MessageView
var udpBenchmarkFull udpBenchmarkDecoded
var udpBenchmarkRaw []byte

// One packet in flight prevents silent loss. These complete delivery costs
// include loopback and a test-only channel acknowledgement, not UDP saturation.
func BenchmarkOwnedUDP(b *testing.B) {
	fixtures := []string{"small", "wide", "long", "array", "bytes"}
	lanes := []string{"raw", "view_selected", "view_full", "json_selected", "json_full", "raw_forward", "view_forward", "view_copy", "json_forward"}
	if os.Getenv("PLASMITE_GO_REVERSE") == "1" {
		reverseBenchmarkStrings(fixtures)
		reverseBenchmarkStrings(lanes)
	}
	for _, fixture := range fixtures {
		b.Run(fixture, func(b *testing.B) {
			for _, lane := range lanes {
				b.Run(lane, func(b *testing.B) {
					client, err := local.NewClient(filepath.Join(b.TempDir(), "pools"))
					if err != nil {
						b.Fatal(err)
					}
					defer client.Close()
					seedAPI, err := client.CreatePool(local.PoolRefName("seed"), 1<<20)
					if err != nil {
						b.Fatal(err)
					}
					seed := seedAPI.(*local.Pool)
					defer seed.Close()
					source, err := seed.AppendView(udpBenchmarkFixture(fixture), nil)
					if err != nil {
						b.Fatal(err)
					}
					encoded := source.Lite3()
					expected, err := source.DataJSON()
					if err != nil {
						b.Fatal(err)
					}
					destinationAPI, err := client.CreatePool(local.PoolRefName("destination"), uint64(len(encoded)*512+65536))
					if err != nil {
						b.Fatal(err)
					}
					destination := destinationAPI.(*local.Pool)
					defer destination.Close()
					peer, err := net.ListenUDP("udp4", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1)})
					if err != nil {
						b.Fatal(err)
					}
					defer peer.Close()
					if err = peer.SetWriteBuffer(128 * 1024); err != nil {
						b.Fatal(err)
					}
					ack := make(chan struct{})
					stop := make(chan struct{})
					defer close(stop)
					sent := make(chan error, 1)
					go func() {
						sent <- func() error {
							if err := peer.SetDeadline(time.Now().Add(20 * time.Second)); err != nil {
								return err
							}
							request := make([]byte, 4096)
							n, address, e := peer.ReadFromUDP(request)
							if e != nil {
								return e
							}
							if n < 24 {
								return fmt.Errorf("short subscription")
							}
							header := append([]byte{0, 'P', 'U', 'D', 'P', 1, 4, 0}, request[8:24]...)
							progress := append(header, []byte(`{"start_seq":1,"next_seq":1,"lease_ms":30000,"terminal":false}`)...)
							if _, e = peer.WriteToUDP(progress, address); e != nil {
								return e
							}
							packet := make([]byte, 40+len(encoded))
							copy(packet, header)
							packet[6] = 7
							copy(packet[40:], encoded)
							binary.BigEndian.PutUint64(packet[32:40], 123)
							select {
							case <-ack:
							case <-stop:
								return nil
							}
							for i := 1; i <= b.N; i++ {
								binary.BigEndian.PutUint64(packet[24:32], uint64(i))
								if _, e = peer.WriteToUDP(packet, address); e != nil {
									return e
								}
								select {
								case <-ack:
								case <-stop:
									return nil
								}
							}
							return nil
						}()
					}()
					udpClient, err := NewClient("udp://" + peer.LocalAddr().String())
					if err != nil {
						b.Fatal(err)
					}
					defer udpClient.Close()
					pool, err := udpClient.OpenPool("bench")
					if err != nil {
						b.Fatal(err)
					}
					defer pool.Close()
					var follow *Follow
					if strings.HasPrefix(lane, "view") {
						follow, err = pool.FollowViews(NewFollowOptions())
					} else if strings.HasPrefix(lane, "raw") {
						follow, err = pool.FollowRaw(NewFollowOptions())
					} else {
						follow, err = pool.Follow(NewFollowOptions())
					}
					if err != nil {
						b.Fatal(err)
					}
					defer follow.Close()
					ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
					defer cancel()
					if event, e := follow.Next(ctx); e != nil {
						b.Fatal(e)
					} else if _, ok := event.(*Progress); !ok {
						b.Fatal("missing opening progress")
					}
					b.ReportAllocs()
					calls := runtime.NumCgoCall()
					b.ResetTimer()
					ack <- struct{}{}
					for i := 1; i <= b.N; i++ {
						event, e := follow.Next(ctx)
						if e != nil {
							b.Fatal(e)
						}
						switch frame := event.(type) {
						case *ViewFrame:
							if frame.Seq != uint64(i) {
								b.Fatal("lost/reordered DATA")
							}
							switch lane {
							case "view_selected":
								var out struct{ Value int }
								e = frame.View.Decode(&out)
								udpBenchmarkValue = out.Value
							case "view_full":
								var out udpBenchmarkDecoded
								e = frame.View.Decode(&out)
								udpBenchmarkValue = out.Value
								udpBenchmarkFull = out
							case "view_forward":
								udpBenchmarkView, e = destination.AppendView(frame.View, nil)
							case "view_copy":
								udpBenchmarkView, e = destination.AppendView(frame.View.Data(), nil)
							}
						case *Frame:
							if frame.Seq != uint64(i) {
								b.Fatal("lost/reordered DATA")
							}
							switch lane {
							case "raw":
								udpBenchmarkValue = len(frame.Payload)
								udpBenchmarkRaw = frame.Payload
							case "raw_forward":
								_, e = destination.AppendLite3(frame.Payload, local.DurabilityFast)
							case "json_selected":
								var out struct{ Value int }
								e = json.Unmarshal(frame.Message.Data, &out)
								udpBenchmarkValue = out.Value
							case "json_full":
								var out udpBenchmarkDecoded
								e = json.Unmarshal(frame.Message.Data, &out)
								udpBenchmarkValue = out.Value
								udpBenchmarkFull = out
							case "json_forward":
								udpBenchmarkView, e = destination.AppendJSONView(frame.Message.Data, nil)
							}
						default:
							b.Fatal("unexpected control event")
						}
						if e != nil {
							b.Fatal(e)
						}
						ack <- struct{}{}
					}
					b.StopTimer()
					b.ReportMetric(float64(runtime.NumCgoCall()-calls)/float64(b.N), "cgo/op")
					if err = <-sent; err != nil {
						b.Fatal(err)
					}
					if strings.HasSuffix(lane, "full") {
						var want udpBenchmarkDecoded
						if e := json.Unmarshal(expected, &want); e != nil {
							b.Fatal(e)
						}
						if !reflect.DeepEqual(want, udpBenchmarkFull) {
							b.Fatal("full conversion mismatch")
						}
					}
					if lane == "raw" && !bytes.Equal(udpBenchmarkRaw, encoded) {
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
							b.Fatal("forwarded value changed")
						}
						if (lane == "raw_forward" || lane == "view_forward") && !bytes.Equal(final.Lite3(), encoded) {
							b.Fatal("forwarded bytes changed")
						}
					} else if lane != "raw" && udpBenchmarkValue != 7 {
						b.Fatal("wrong selected value")
					}
				})
			}
		})
	}
}

type udpBenchmarkDecoded struct {
	Value int
	Extra any
	Blob  []byte
}

func udpBenchmarkFixture(fixture string) map[string]any {
	value := map[string]any{"value": 7, "extra": "sensor"}
	switch fixture {
	case "wide":
		fields := map[string]any{}
		for i := 0; i < 200; i++ {
			fields[fmt.Sprintf("field_%03d", i)] = i
		}
		value["extra"] = fields
	case "long":
		value["extra"] = strings.Repeat("x", 32000)
	case "array":
		items := make([]int, 1000)
		for i := range items {
			items[i] = i
		}
		value["extra"] = items
	case "bytes":
		value["blob"] = []byte(strings.Repeat("x", 32000))
	}
	return value
}
func reverseBenchmarkStrings(values []string) {
	for i, j := 0, len(values)-1; i < j; i, j = i+1, j-1 {
		values[i], values[j] = values[j], values[i]
	}
}
