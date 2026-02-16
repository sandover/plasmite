/*
Purpose: Execute conformance manifests against the Go binding.
Key Exports: None (command entry point).
Role: Reference runner for JSON conformance manifests in Go.
Invariants: Manifests are JSON-only; steps execute in order; fail-fast on errors.
Invariants: Workdir is isolated under the manifest directory.
Notes: Mirrors the Rust conformance runner behavior.
*/
package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"runtime"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	plasmite "github.com/sandover/plasmite/bindings/go/plasmite/local"
)

type message struct {
	Seq  uint64 `json:"seq"`
	Meta struct {
		Tags []string `json:"tags"`
	} `json:"meta"`
	Data any `json:"data"`
}

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run() error {
	if len(os.Args) != 2 {
		return errors.New("usage: plasmite-conformance <path/to/manifest.json>")
	}
	manifestPath := os.Args[1]
	manifestDir := filepath.Dir(manifestPath)
	repoRoot := filepath.Dir(manifestDir)

	content, err := os.ReadFile(manifestPath)
	if err != nil {
		return fmt.Errorf("failed to read manifest: %w", err)
	}

	var manifest map[string]any
	if err := json.Unmarshal(content, &manifest); err != nil {
		return fmt.Errorf("failed to parse manifest json: %w", err)
	}

	version, ok := manifest["conformance_version"].(float64)
	if !ok {
		return errors.New("missing conformance_version")
	}
	if int(version) != 0 {
		return fmt.Errorf("unsupported conformance_version: %v", int(version))
	}

	workdir := "work"
	if raw, ok := manifest["workdir"].(string); ok && raw != "" {
		workdir = raw
	}
	workdirPath := filepath.Join(manifestDir, workdir)
	if err := resetWorkdir(workdirPath); err != nil {
		return err
	}

	steps, ok := manifest["steps"].([]any)
	if !ok {
		return errors.New("manifest steps must be an array")
	}

	client, err := plasmite.NewClient(workdirPath)
	if err != nil {
		return fmt.Errorf("client init failed: %w", err)
	}
	defer client.Close()

	for index, stepValue := range steps {
		step, ok := stepValue.(map[string]any)
		if !ok {
			return stepErr(index, nil, "step must be an object")
		}
		var stepID *string
		if raw, ok := step["id"].(string); ok {
			stepID = &raw
		}
		op, ok := step["op"].(string)
		if !ok {
			return stepErr(index, stepID, "missing op")
		}
		switch op {
		case "create_pool":
			if err := runCreatePool(client, step, index, stepID); err != nil {
				return err
			}
		case "append":
			if err := runAppend(client, step, index, stepID); err != nil {
				return err
			}
		case "fetch":
			if err := runGet(client, step, index, stepID); err != nil {
				return err
			}
		case "tail":
			if err := runTail(client, step, index, stepID); err != nil {
				return err
			}
		case "list_pools":
			if err := runListPools(step, index, stepID, workdirPath); err != nil {
				return err
			}
		case "pool_info":
			if err := runPoolInfo(repoRoot, workdirPath, step, index, stepID); err != nil {
				return err
			}
		case "delete_pool":
			if err := runDeletePool(step, index, stepID, workdirPath); err != nil {
				return err
			}
		case "spawn_poke":
			if err := runSpawnPoke(repoRoot, workdirPath, step, index, stepID); err != nil {
				return err
			}
		case "corrupt_pool_header":
			if err := runCorruptPoolHeader(step, index, stepID, workdirPath); err != nil {
				return err
			}
		case "chmod_path":
			if err := runChmodPath(step, index, stepID); err != nil {
				return err
			}
		default:
			return stepErr(index, stepID, fmt.Sprintf("unknown op: %s", op))
		}
	}

	return nil
}

func resetWorkdir(path string) error {
	if err := os.RemoveAll(path); err != nil {
		return fmt.Errorf("failed to clear workdir %s: %w", path, err)
	}
	if err := os.MkdirAll(path, 0o755); err != nil {
		return fmt.Errorf("failed to create workdir %s: %w", path, err)
	}
	return nil
}

func runCreatePool(client *plasmite.Client, step map[string]any, index int, stepID *string) error {
	poolRef, err := poolRefFromStep(step, index, stepID)
	if err != nil {
		return err
	}

	sizeBytes := uint64(1024 * 1024)
	if input, ok := step["input"].(map[string]any); ok {
		if raw, ok := input["size_bytes"].(float64); ok {
			sizeBytes = uint64(raw)
		}
	}

	_, err = client.CreatePool(poolRef, sizeBytes)
	return validateExpectError(step["expect"], err, index, stepID)
}

func runAppend(client *plasmite.Client, step map[string]any, index int, stepID *string) error {
	poolRef, err := poolRefFromStep(step, index, stepID)
	if err != nil {
		return err
	}
	pool, err := client.OpenPool(poolRef)
	if err != nil {
		return validateExpectError(step["expect"], err, index, stepID)
	}
	defer pool.Close()

	input, ok := step["input"].(map[string]any)
	if !ok {
		return stepErr(index, stepID, "missing input")
	}
	data, ok := input["data"]
	if !ok {
		return stepErr(index, stepID, "missing input.data")
	}
	var tags []string
	if raw, ok := input["tags"].([]any); ok {
		tags, err = parseStringArray(raw)
		if err != nil {
			return stepErr(index, stepID, err.Error())
		}
	}

	messageBytes, err := pool.Append(data, tags, plasmite.DurabilityFast)
	if err != nil {
		return validateExpectError(step["expect"], err, index, stepID)
	}
	if err := validateExpectError(step["expect"], nil, index, stepID); err != nil {
		return err
	}

	if expect, ok := step["expect"].(map[string]any); ok {
		if rawSeq, ok := expect["seq"].(float64); ok {
			msg, err := parseMessage(messageBytes)
			if err != nil {
				return stepErr(index, stepID, fmt.Sprintf("failed to parse message: %v", err))
			}
			if msg.Seq != uint64(rawSeq) {
				return stepErr(index, stepID, fmt.Sprintf("expected seq %.0f, got %d", rawSeq, msg.Seq))
			}
		}
	}

	return nil
}

func runGet(client *plasmite.Client, step map[string]any, index int, stepID *string) error {
	poolRef, err := poolRefFromStep(step, index, stepID)
	if err != nil {
		return err
	}
	pool, err := client.OpenPool(poolRef)
	if err != nil {
		return validateExpectError(step["expect"], err, index, stepID)
	}
	defer pool.Close()

	input, ok := step["input"].(map[string]any)
	if !ok {
		return stepErr(index, stepID, "missing input")
	}
	seqRaw, ok := input["seq"].(float64)
	if !ok {
		return stepErr(index, stepID, "missing input.seq")
	}

	messageBytes, err := pool.Get(uint64(seqRaw))
	if err != nil {
		return validateExpectError(step["expect"], err, index, stepID)
	}
	if err := validateExpectError(step["expect"], nil, index, stepID); err != nil {
		return err
	}

	msg, err := parseMessage(messageBytes)
	if err != nil {
		return stepErr(index, stepID, fmt.Sprintf("failed to parse message: %v", err))
	}
	if err := expectData(step, msg.Data, index, stepID); err != nil {
		return err
	}
	if err := expectTags(step, msg.Meta.Tags, index, stepID); err != nil {
		return err
	}

	return nil
}

func runTail(client *plasmite.Client, step map[string]any, index int, stepID *string) error {
	poolRef, err := poolRefFromStep(step, index, stepID)
	if err != nil {
		return err
	}
	pool, err := client.OpenPool(poolRef)
	if err != nil {
		return validateExpectError(step["expect"], err, index, stepID)
	}
	defer pool.Close()

	input, _ := step["input"].(map[string]any)
	var since *uint64
	var max *uint64
	if input != nil {
		if raw, ok := input["since_seq"].(float64); ok {
			val := uint64(raw)
			since = &val
		}
		if raw, ok := input["max"].(float64); ok {
			val := uint64(raw)
			max = &val
		}
	}

	expectMessages, ordered, err := expectedMessages(step, index, stepID)
	if err != nil {
		return err
	}
	if max == nil {
		val := uint64(len(expectMessages))
		max = &val
	}

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	out, errs := pool.Tail(ctx, plasmite.TailOptions{
		SinceSeq:    since,
		MaxMessages: max,
		Timeout:     500 * time.Millisecond,
		Buffer:      8,
	})

	var messages []message
	for msg := range out {
		parsed, err := parseMessage(msg)
		if err != nil {
			return stepErr(index, stepID, fmt.Sprintf("failed to parse message: %v", err))
		}
		messages = append(messages, parsed)
		if uint64(len(messages)) >= *max {
			break
		}
	}
	select {
	case err := <-errs:
		if err != nil && !errors.Is(err, context.DeadlineExceeded) {
			return validateExpectError(step["expect"], err, index, stepID)
		}
	default:
	}

	if len(messages) != len(expectMessages) {
		return stepErr(index, stepID, fmt.Sprintf("expected %d messages, got %d", len(expectMessages), len(messages)))
	}
	for i := 1; i < len(messages); i++ {
		if messages[i-1].Seq >= messages[i].Seq {
			return stepErr(index, stepID, "tail messages out of order")
		}
	}
	if ordered {
		for idx, expected := range expectMessages {
			actual := messages[idx]
			if !reflect.DeepEqual(expected.Data, actual.Data) {
				return stepErr(index, stepID, "data mismatch")
			}
			if expected.Tags != nil && !reflect.DeepEqual(expected.Tags, actual.Meta.Tags) {
				return stepErr(index, stepID, "tags mismatch")
			}
		}
	} else {
		used := make([]bool, len(messages))
		for _, expected := range expectMessages {
			found := false
			for idx, actual := range messages {
				if used[idx] {
					continue
				}
				if !reflect.DeepEqual(expected.Data, actual.Data) {
					continue
				}
				if expected.Tags != nil && !reflect.DeepEqual(expected.Tags, actual.Meta.Tags) {
					continue
				}
				used[idx] = true
				found = true
				break
			}
			if !found {
				return stepErr(index, stepID, "message mismatch")
			}
		}
	}

	return validateExpectError(step["expect"], nil, index, stepID)
}

func runListPools(step map[string]any, index int, stepID *string, workdirPath string) error {
	names, err := listPoolNames(workdirPath)
	if err != nil {
		return validateExpectError(step["expect"], err, index, stepID)
	}
	if err := validateExpectError(step["expect"], nil, index, stepID); err != nil {
		return err
	}

	if expect, ok := step["expect"].(map[string]any); ok {
		if rawNames, ok := expect["names"]; ok {
			list, ok := rawNames.([]any)
			if !ok {
				return stepErr(index, stepID, "expect.names must be array")
			}
			expected, err := parseStringArray(list)
			if err != nil {
				return stepErr(index, stepID, err.Error())
			}
			actual := append([]string{}, names...)
			sort.Strings(actual)
			sort.Strings(expected)
			if !reflect.DeepEqual(actual, expected) {
				return stepErr(index, stepID, "pool list mismatch")
			}
		}
	}

	return nil
}

func runPoolInfo(repoRoot string, workdirPath string, step map[string]any, index int, stepID *string) error {
	pool, ok := step["pool"].(string)
	if !ok {
		return stepErr(index, stepID, "missing pool")
	}
	bin, err := resolvePlasmiteBin(repoRoot)
	if err != nil {
		return stepErr(index, stepID, err.Error())
	}
	cmd := exec.Command(bin, "--dir", workdirPath, "pool", "info", pool, "--json")
	output, err := cmd.Output()
	if err != nil {
		if exitErr, ok := err.(*exec.ExitError); ok {
			parsed, parseErr := parseErrorJSON(exitErr.Stderr)
			if parseErr != nil {
				return stepErr(index, stepID, fmt.Sprintf("pool info failed: %v", err))
			}
			return validateExpectError(step["expect"], parsed, index, stepID)
		}
		return stepErr(index, stepID, fmt.Sprintf("pool info failed: %v", err))
	}
	if err := validateExpectError(step["expect"], nil, index, stepID); err != nil {
		return err
	}
	info, err := parsePoolInfo(output)
	if err != nil {
		return stepErr(index, stepID, fmt.Sprintf("parse pool info failed: %v", err))
	}

	if expect, ok := step["expect"].(map[string]any); ok {
		if raw, ok := expect["file_size"].(float64); ok {
			if info.FileSize != uint64(raw) {
				return stepErr(index, stepID, "file_size mismatch")
			}
		}
		if raw, ok := expect["ring_size"].(float64); ok {
			if info.RingSize != uint64(raw) {
				return stepErr(index, stepID, "ring_size mismatch")
			}
		}
		if bounds, ok := expect["bounds"].(map[string]any); ok {
			if err := expectBounds(bounds, info.Bounds, index, stepID); err != nil {
				return err
			}
		}
	}

	return nil
}

func runDeletePool(step map[string]any, index int, stepID *string, workdirPath string) error {
	pool, ok := step["pool"].(string)
	if !ok {
		return stepErr(index, stepID, "missing pool")
	}
	path := resolvePoolPath(pool, workdirPath)
	if err := os.Remove(path); err != nil {
		return validateExpectError(step["expect"], mapIOError(err, path, "failed to delete pool"), index, stepID)
	}
	return validateExpectError(step["expect"], nil, index, stepID)
}

func runSpawnPoke(repoRoot string, workdirPath string, step map[string]any, index int, stepID *string) error {
	pool, ok := step["pool"].(string)
	if !ok {
		return stepErr(index, stepID, "missing pool")
	}
	input, ok := step["input"].(map[string]any)
	if !ok {
		return stepErr(index, stepID, "missing input")
	}
	rawMessages, ok := input["messages"].([]any)
	if !ok {
		return stepErr(index, stepID, "input.messages must be array")
	}
	bin, err := resolvePlasmiteBin(repoRoot)
	if err != nil {
		return stepErr(index, stepID, err.Error())
	}

	var wg sync.WaitGroup
	errs := make(chan error, len(rawMessages))
	for _, raw := range rawMessages {
		entry, ok := raw.(map[string]any)
		if !ok {
			return stepErr(index, stepID, "message must be object")
		}
		data, ok := entry["data"]
		if !ok {
			return stepErr(index, stepID, "message.data is required")
		}
		payload, err := json.Marshal(data)
		if err != nil {
			return stepErr(index, stepID, fmt.Sprintf("encode payload failed: %v", err))
		}
		var tags []string
		if rawTags, ok := entry["tags"].([]any); ok {
			tags, err = parseStringArray(rawTags)
			if err != nil {
				return stepErr(index, stepID, err.Error())
			}
		}

		wg.Add(1)
		go func(payload string, tags []string) {
			defer wg.Done()
			args := []string{"--dir", workdirPath, "feed", pool, payload}
			for _, tag := range tags {
				args = append(args, "--tag", tag)
			}
			cmd := exec.Command(bin, args...)
			cmd.Stdout = os.Stdout
			cmd.Stderr = os.Stderr
			if err := cmd.Run(); err != nil {
				errs <- err
			}
		}(string(payload), tags)
	}

	wg.Wait()
	close(errs)
	if err := <-errs; err != nil {
		return stepErr(index, stepID, fmt.Sprintf("feed process failed: %v", err))
	}
	return nil
}

func runCorruptPoolHeader(step map[string]any, index int, stepID *string, workdirPath string) error {
	poolRefRaw, ok := step["pool"].(string)
	if !ok {
		return stepErr(index, stepID, "missing pool")
	}
	path := resolvePoolPath(poolRefRaw, workdirPath)
	if err := os.WriteFile(path, []byte("NOPE"), 0o600); err != nil {
		return stepErr(index, stepID, fmt.Sprintf("failed to corrupt pool header: %v", err))
	}
	return nil
}

func runChmodPath(step map[string]any, index int, stepID *string) error {
	if runtime.GOOS == "windows" {
		return stepErr(index, stepID, "chmod_path is not supported on this platform")
	}
	input, ok := step["input"].(map[string]any)
	if !ok {
		return stepErr(index, stepID, "missing input")
	}
	path, ok := input["path"].(string)
	if !ok {
		return stepErr(index, stepID, "missing input.path")
	}
	modeRaw, ok := input["mode"].(string)
	if !ok {
		return stepErr(index, stepID, "missing input.mode")
	}
	mode, err := strconv.ParseUint(modeRaw, 8, 32)
	if err != nil {
		return stepErr(index, stepID, "invalid input.mode")
	}
	if err := os.Chmod(path, os.FileMode(mode)); err != nil {
		return stepErr(index, stepID, fmt.Sprintf("chmod failed: %v", err))
	}
	return nil
}

type poolInfo struct {
	FileSize uint64
	RingSize uint64
	Bounds   poolBounds
}

type poolBounds struct {
	Oldest *uint64
	Newest *uint64
}

func listPoolNames(workdirPath string) ([]string, error) {
	entries, err := os.ReadDir(workdirPath)
	if err != nil {
		return nil, mapIOError(err, workdirPath, "failed to read pool directory")
	}
	var names []string
	for _, entry := range entries {
		name := entry.Name()
		if strings.HasSuffix(name, ".plasmite") {
			names = append(names, strings.TrimSuffix(name, ".plasmite"))
		}
	}
	return names, nil
}

func mapIOError(err error, path string, message string) *plasmite.Error {
	kind := plasmite.ErrorIO
	if os.IsNotExist(err) {
		kind = plasmite.ErrorNotFound
	} else if os.IsPermission(err) {
		kind = plasmite.ErrorPermission
	}
	return &plasmite.Error{
		Kind:    kind,
		Message: message,
		Path:    path,
	}
}

func parseErrorJSON(data []byte) (*plasmite.Error, error) {
	if len(data) == 0 {
		return nil, errors.New("empty error output")
	}
	var payload map[string]any
	if err := json.Unmarshal(data, &payload); err != nil {
		return nil, err
	}
	rawErr, ok := payload["error"].(map[string]any)
	if !ok {
		return nil, errors.New("missing error object")
	}
	kindLabel, _ := rawErr["kind"].(string)
	message, _ := rawErr["message"].(string)
	path, _ := rawErr["path"].(string)
	seq, err := parseOptionalUint(rawErr["seq"])
	if err != nil {
		return nil, err
	}
	offset, err := parseOptionalUint(rawErr["offset"])
	if err != nil {
		return nil, err
	}
	return &plasmite.Error{
		Kind:    errorKindFromString(kindLabel),
		Message: message,
		Path:    path,
		Seq:     seq,
		Offset:  offset,
	}, nil
}

func errorKindFromString(kind string) plasmite.ErrorKind {
	switch kind {
	case "Internal":
		return plasmite.ErrorInternal
	case "Usage":
		return plasmite.ErrorUsage
	case "NotFound":
		return plasmite.ErrorNotFound
	case "AlreadyExists":
		return plasmite.ErrorAlreadyExists
	case "Busy":
		return plasmite.ErrorBusy
	case "Permission":
		return plasmite.ErrorPermission
	case "Corrupt":
		return plasmite.ErrorCorrupt
	case "Io":
		return plasmite.ErrorIO
	default:
		return plasmite.ErrorInternal
	}
}

func parsePoolInfo(data []byte) (poolInfo, error) {
	var payload map[string]any
	if err := json.Unmarshal(data, &payload); err != nil {
		return poolInfo{}, err
	}
	fileSizeRaw, ok := payload["file_size"].(float64)
	if !ok {
		return poolInfo{}, errors.New("missing file_size")
	}
	ringSizeRaw, ok := payload["ring_size"].(float64)
	if !ok {
		return poolInfo{}, errors.New("missing ring_size")
	}
	var bounds poolBounds
	if rawBounds, ok := payload["bounds"].(map[string]any); ok {
		oldest, err := parseOptionalUint(rawBounds["oldest"])
		if err != nil {
			return poolInfo{}, err
		}
		newest, err := parseOptionalUint(rawBounds["newest"])
		if err != nil {
			return poolInfo{}, err
		}
		bounds.Oldest = oldest
		bounds.Newest = newest
	}
	return poolInfo{
		FileSize: uint64(fileSizeRaw),
		RingSize: uint64(ringSizeRaw),
		Bounds:   bounds,
	}, nil
}

func parseOptionalUint(value any) (*uint64, error) {
	if value == nil {
		return nil, nil
	}
	raw, ok := value.(float64)
	if !ok {
		return nil, errors.New("expected number or null")
	}
	out := uint64(raw)
	return &out, nil
}

func expectBounds(expect map[string]any, actual poolBounds, index int, stepID *string) error {
	if raw, ok := expect["oldest"]; ok {
		expected, err := parseOptionalUint(raw)
		if err != nil {
			return stepErr(index, stepID, "bounds.oldest must be number or null")
		}
		if !uintPtrEqual(expected, actual.Oldest) {
			return stepErr(index, stepID, "bounds.oldest mismatch")
		}
	}
	if raw, ok := expect["newest"]; ok {
		expected, err := parseOptionalUint(raw)
		if err != nil {
			return stepErr(index, stepID, "bounds.newest must be number or null")
		}
		if !uintPtrEqual(expected, actual.Newest) {
			return stepErr(index, stepID, "bounds.newest mismatch")
		}
	}
	return nil
}

func uintPtrEqual(left *uint64, right *uint64) bool {
	if left == nil && right == nil {
		return true
	}
	if left == nil || right == nil {
		return false
	}
	return *left == *right
}

func validateExpectError(expect any, resultErr error, index int, stepID *string) error {
	expectMap, ok := expect.(map[string]any)
	if !ok {
		if resultErr == nil {
			return nil
		}
		return stepErr(index, stepID, fmt.Sprintf("unexpected error: %v", resultErr))
	}

	expectErr, ok := expectMap["error"].(map[string]any)
	if !ok {
		if resultErr == nil {
			return nil
		}
		return stepErr(index, stepID, fmt.Sprintf("unexpected error: %v", resultErr))
	}

	if resultErr == nil {
		return stepErr(index, stepID, "expected error but operation succeeded")
	}
	var pe *plasmite.Error
	if !errors.As(resultErr, &pe) {
		return stepErr(index, stepID, fmt.Sprintf("unexpected error type: %v", resultErr))
	}

	kind, ok := expectErr["kind"].(string)
	if !ok {
		return stepErr(index, stepID, "expect.error.kind is required")
	}
	if kind != errorKindLabel(pe.Kind) {
		return stepErr(index, stepID, fmt.Sprintf("expected error kind %s, got %s", kind, errorKindLabel(pe.Kind)))
	}
	if substr, ok := expectErr["message_contains"].(string); ok {
		if !strings.Contains(pe.Message, substr) {
			return stepErr(index, stepID, fmt.Sprintf("expected message to contain '%s', got '%s'", substr, pe.Message))
		}
	}
	if hasPath, ok := expectErr["has_path"].(bool); ok {
		if hasPath != (pe.Path != "") {
			return stepErr(index, stepID, "path presence mismatch")
		}
	}
	if hasSeq, ok := expectErr["has_seq"].(bool); ok {
		if hasSeq != (pe.Seq != nil) {
			return stepErr(index, stepID, "seq presence mismatch")
		}
	}
	if hasOffset, ok := expectErr["has_offset"].(bool); ok {
		if hasOffset != (pe.Offset != nil) {
			return stepErr(index, stepID, "offset presence mismatch")
		}
	}

	return nil
}

func errorKindLabel(kind plasmite.ErrorKind) string {
	switch kind {
	case plasmite.ErrorInternal:
		return "Internal"
	case plasmite.ErrorUsage:
		return "Usage"
	case plasmite.ErrorNotFound:
		return "NotFound"
	case plasmite.ErrorAlreadyExists:
		return "AlreadyExists"
	case plasmite.ErrorBusy:
		return "Busy"
	case plasmite.ErrorPermission:
		return "Permission"
	case plasmite.ErrorCorrupt:
		return "Corrupt"
	case plasmite.ErrorIO:
		return "Io"
	default:
		return "Internal"
	}
}

func poolRefFromStep(step map[string]any, index int, stepID *string) (plasmite.PoolRef, error) {
	pool, ok := step["pool"].(string)
	if !ok {
		return "", stepErr(index, stepID, "missing pool")
	}
	return poolRefFromValue(pool)
}

func poolRefFromValue(pool string) (plasmite.PoolRef, error) {
	if strings.Contains(pool, "/") {
		return plasmite.PoolRefPath(pool), nil
	}
	return plasmite.PoolRefName(pool), nil
}

func resolvePoolPath(pool string, workdirPath string) string {
	if strings.Contains(pool, "/") {
		return pool
	}
	if strings.HasSuffix(pool, ".plasmite") {
		return filepath.Join(workdirPath, pool)
	}
	return filepath.Join(workdirPath, pool+".plasmite")
}

func parseMessage(payload []byte) (message, error) {
	var msg message
	if err := json.Unmarshal(payload, &msg); err != nil {
		return message{}, err
	}
	return msg, nil
}

func parseStringArray(values []any) ([]string, error) {
	out := make([]string, len(values))
	for i, value := range values {
		str, ok := value.(string)
		if !ok {
			return nil, errors.New("expected string array")
		}
		out[i] = str
	}
	return out, nil
}

func expectedMessages(step map[string]any, index int, stepID *string) ([]messageExpectation, bool, error) {
	expect, ok := step["expect"].(map[string]any)
	if !ok {
		return nil, false, stepErr(index, stepID, "missing expect")
	}
	rawMessages, ok := expect["messages"].([]any)
	ordered := ok
	if ok {
		if _, hasUnordered := expect["messages_unordered"]; hasUnordered {
			return nil, false, stepErr(index, stepID, "expect.messages and expect.messages_unordered are mutually exclusive")
		}
	} else {
		rawMessages, ok = expect["messages_unordered"].([]any)
		if !ok {
			return nil, false, stepErr(index, stepID, "expect.messages or expect.messages_unordered is required")
		}
	}
	messages := make([]messageExpectation, len(rawMessages))
	for i, raw := range rawMessages {
		entry, ok := raw.(map[string]any)
		if !ok {
			return nil, false, stepErr(index, stepID, "message must be object")
		}
		messages[i] = messageExpectation{Data: entry["data"]}
		if rawTags, ok := entry["tags"].([]any); ok {
			tags, err := parseStringArray(rawTags)
			if err != nil {
				return nil, false, stepErr(index, stepID, err.Error())
			}
			messages[i].Tags = tags
		}
	}
	return messages, ordered, nil
}

func resolvePlasmiteBin(repoRoot string) (string, error) {
	if value := os.Getenv("PLASMITE_BIN"); value != "" {
		return value, nil
	}
	candidate := filepath.Join(repoRoot, "target", "debug", "plasmite")
	if _, err := os.Stat(candidate); err == nil {
		return candidate, nil
	}
	return "", errors.New("plasmite binary not found; set PLASMITE_BIN or build target/debug/plasmite")
}

type messageExpectation struct {
	Data any
	Tags []string
}

func expectData(step map[string]any, actual any, index int, stepID *string) error {
	expect, ok := step["expect"].(map[string]any)
	if !ok {
		return nil
	}
	if expected, ok := expect["data"]; ok {
		if !reflect.DeepEqual(expected, actual) {
			return stepErr(index, stepID, "data mismatch")
		}
	}
	return nil
}

func expectTags(step map[string]any, actual []string, index int, stepID *string) error {
	expect, ok := step["expect"].(map[string]any)
	if !ok {
		return nil
	}
	if expectedRaw, ok := expect["tags"]; ok {
		expectedList, ok := expectedRaw.([]any)
		if !ok {
			return stepErr(index, stepID, "tags must be array")
		}
		expected, err := parseStringArray(expectedList)
		if err != nil {
			return stepErr(index, stepID, err.Error())
		}
		if !reflect.DeepEqual(expected, actual) {
			return stepErr(index, stepID, "tags mismatch")
		}
	}
	return nil
}

func stepErr(index int, stepID *string, message string) error {
	out := fmt.Sprintf("step %d", index)
	if stepID != nil {
		out = fmt.Sprintf("%s (%s)", out, *stepID)
	}
	return errors.New(out + ": " + message)
}
