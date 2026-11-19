// Persistent local Go worker for bench_language_boundaries.py.
// Build from bindings/go; fixture decoding and protocol I/O sit outside call timings.
package main

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"reflect"
	"runtime"
	"time"

	"github.com/sandover/plasmite/bindings/go/local"
)

type request struct {
	Op        string   `json:"op"`
	Items     []any    `json:"items"`
	Expected  []any    `json:"expected"`
	Sequences []uint64 `json:"sequences"`
}

func run() error {
	if len(os.Args) != 3 {
		return fmt.Errorf("usage: bench_boundary_go DIRECTORY POOL")
	}
	client, err := local.NewClient(os.Args[1])
	if err != nil {
		return err
	}
	defer client.Close()
	pool, err := client.OpenPool(local.PoolRefName(os.Args[2]))
	if err != nil {
		return err
	}
	defer pool.Close()
	input, output := json.NewDecoder(os.Stdin), json.NewEncoder(os.Stdout)
	for {
		var req request
		if err := input.Decode(&req); err == io.EOF {
			return nil
		} else if err != nil {
			return err
		}
		if req.Op != "append" && req.Op != "read" {
			return fmt.Errorf("unsupported operation %q", req.Op)
		}
		if req.Op == "read" && len(req.Expected) != len(req.Sequences) {
			return fmt.Errorf("read count mismatch")
		}
		count := len(req.Items)
		if req.Op == "read" {
			count = len(req.Sequences)
		}
		timings := make([]int64, 0, count)
		sequences := make([]uint64, 0, count)
		var before, after runtime.MemStats
		if os.Getenv("PLASMITE_BENCH_DIAGNOSTICS") == "1" {
			runtime.ReadMemStats(&before)
		}
		started := time.Now()
		for i := 0; i < count; i++ {
			call := time.Now()
			if req.Op == "append" {
				message, err := pool.Feed(req.Items[i], nil)
				if err != nil {
					return err
				}
				timings = append(timings, time.Since(call).Nanoseconds())
				sequences = append(sequences, message.Seq)
			} else {
				reader, err := pool.Fetch(local.Since(req.Sequences[i]))
				if err != nil {
					return err
				}
				message, err := reader.Next(context.Background())
				reader.Close()
				if err != nil {
					return err
				}
				var value any
				if err := json.Unmarshal(message.Data, &value); err != nil {
					return err
				}
				timings = append(timings, time.Since(call).Nanoseconds())
				if message.Seq != req.Sequences[i] || !reflect.DeepEqual(value, req.Expected[i]) {
					return fmt.Errorf("readback mismatch at %d", i)
				}
			}
		}
		elapsed := time.Since(started).Nanoseconds()
		if os.Getenv("PLASMITE_BENCH_DIAGNOSTICS") == "1" {
			runtime.ReadMemStats(&after)
		}
		result := map[string]any{"verified_count": count, "sequences": sequences}
		response := map[string]any{"latencies_ns": timings, "elapsed_ns": elapsed, "result": result,
			"diagnostics": map[string]any{"go_mallocs": after.Mallocs - before.Mallocs, "go_allocated_bytes": after.TotalAlloc - before.TotalAlloc, "go_gc_cycles": after.NumGC - before.NumGC}}
		if err := output.Encode(response); err != nil {
			return err
		}
	}
}
func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
