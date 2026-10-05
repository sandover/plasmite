package main

import (
	"context"
	"os"
	"path/filepath"
	"runtime"
	"testing"
	"time"

	"github.com/sandover/plasmite/bindings/go/api"
	plasmite "github.com/sandover/plasmite/bindings/go/local"
)

func TestRetentionGapConformance(t *testing.T) {
	client, err := plasmite.NewClient(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	if err := runRetentionGap(client, map[string]any{"pool": "gap"}, 0, nil); err != nil {
		t.Fatal(err)
	}
}

func TestDrainBeforeRetentionGap(t *testing.T) {
	for _, test := range []struct {
		name       string
		sequences  []uint64
		gap        uint64
		minimum    int
		nilMessage bool
		noError    bool
		wantError  bool
	}{
		{name: "buffered second", sequences: []uint64{2}, gap: 3, minimum: 1},
		{name: "owned pending bridge", sequences: []uint64{2, 3}, gap: 4, minimum: 1},
		{name: "closed empty output", gap: 2},
		{name: "post gap data rejected", sequences: []uint64{2, 3, 4}, gap: 5, minimum: 1, wantError: true},
		{name: "noncontiguous data rejected", sequences: []uint64{3}, gap: 4, minimum: 1, wantError: true},
		{name: "wrong exact gap rejected", sequences: []uint64{2}, gap: 4, minimum: 1, wantError: true},
		{name: "missing buffered second rejected", gap: 2, minimum: 1, wantError: true},
		{name: "nil message rejected", gap: 2, nilMessage: true, wantError: true},
		{name: "missing terminal error rejected", noError: true, wantError: true},
	} {
		t.Run(test.name, func(t *testing.T) {
			ctx, cancel := context.WithTimeout(context.Background(), time.Second)
			defer cancel()
			out := make(chan *api.Message, len(test.sequences)+1)
			for _, seq := range test.sequences {
				out <- &api.Message{Seq: seq}
			}
			if test.nilMessage {
				out <- nil
			}
			close(out)
			errs := make(chan error, 1)
			if !test.noError {
				errs <- &plasmite.Error{Kind: plasmite.ErrorRetentionGap, Seq: &test.gap}
			}
			close(errs)
			err := drainBeforeRetentionGap(ctx, out, errs, 2, 3, test.minimum)
			if (err != nil) != test.wantError {
				t.Fatalf("error = %v, wantError = %v", err, test.wantError)
			}
		})
	}
}

func TestWorkdirBoundary(t *testing.T) {
	for _, name := range []string{"src", "work-", "work.", "work-..", "", ".", "..", "../pools", "/tmp/pools", "a/b", "a\\b", "C:pools", "a\x00b"} {
		if err := validateWorkdirName(name); err == nil {
			t.Fatalf("accepted %q", name)
		}
	}
	if err := validateWorkdirName("work-retention-gap"); err != nil {
		t.Fatal(err)
	}
	if runtime.GOOS == "windows" {
		t.Skip("symlink creation requires privileges on Windows")
	}
	temp := t.TempDir()
	target := filepath.Join(temp, "target")
	if err := os.Mkdir(target, 0o755); err != nil {
		t.Fatal(err)
	}
	marker := filepath.Join(target, "keep")
	if err := os.WriteFile(marker, []byte("unchanged"), 0o600); err != nil {
		t.Fatal(err)
	}
	work := filepath.Join(temp, "work")
	if err := os.Symlink(target, work); err != nil {
		t.Fatal(err)
	}
	if err := resetWorkdir(work); err == nil {
		t.Fatal("accepted workdir symlink")
	}
	content, err := os.ReadFile(marker)
	if err != nil || string(content) != "unchanged" {
		t.Fatalf("target changed: %q, %v", content, err)
	}
}
