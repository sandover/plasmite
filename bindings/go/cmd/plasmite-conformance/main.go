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
	"path/filepath"
	"reflect"
	"runtime"
	"strconv"
	"strings"
	"time"

	"github.com/sandover/plasmite/bindings/go/plasmite"
)

type message struct {
	Seq  uint64 `json:"seq"`
	Meta struct {
		Descrips []string `json:"descrips"`
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
		case "get":
			if err := runGet(client, step, index, stepID); err != nil {
				return err
			}
		case "tail":
			if err := runTail(client, step, index, stepID); err != nil {
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
	var descrips []string
	if raw, ok := input["descrips"].([]any); ok {
		descrips, err = parseStringArray(raw)
		if err != nil {
			return stepErr(index, stepID, err.Error())
		}
	}

	messageBytes, err := pool.Append(data, descrips, plasmite.DurabilityFast)
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
	if err := expectDescrips(step, msg.Meta.Descrips, index, stepID); err != nil {
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

	expectMessages, err := expectedMessages(step, index, stepID)
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
	for idx, expected := range expectMessages {
		actual := messages[idx]
		if !reflect.DeepEqual(expected.Data, actual.Data) {
			return stepErr(index, stepID, "data mismatch")
		}
		if expected.Descrips != nil && !reflect.DeepEqual(expected.Descrips, actual.Meta.Descrips) {
			return stepErr(index, stepID, "descrips mismatch")
		}
	}

	return validateExpectError(step["expect"], nil, index, stepID)
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

func expectedMessages(step map[string]any, index int, stepID *string) ([]messageExpectation, error) {
	expect, ok := step["expect"].(map[string]any)
	if !ok {
		return nil, stepErr(index, stepID, "missing expect")
	}
	rawMessages, ok := expect["messages"].([]any)
	if !ok {
		return nil, stepErr(index, stepID, "expect.messages must be array")
	}
	messages := make([]messageExpectation, len(rawMessages))
	for i, raw := range rawMessages {
		entry, ok := raw.(map[string]any)
		if !ok {
			return nil, stepErr(index, stepID, "message must be object")
		}
		messages[i] = messageExpectation{Data: entry["data"]}
		if rawDescrips, ok := entry["descrips"].([]any); ok {
			descrips, err := parseStringArray(rawDescrips)
			if err != nil {
				return nil, stepErr(index, stepID, err.Error())
			}
			messages[i].Descrips = descrips
		}
	}
	return messages, nil
}

type messageExpectation struct {
	Data     any
	Descrips []string
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

func expectDescrips(step map[string]any, actual []string, index int, stepID *string) error {
	expect, ok := step["expect"].(map[string]any)
	if !ok {
		return nil
	}
	if expectedRaw, ok := expect["descrips"]; ok {
		expectedList, ok := expectedRaw.([]any)
		if !ok {
			return stepErr(index, stepID, "descrips must be array")
		}
		expected, err := parseStringArray(expectedList)
		if err != nil {
			return stepErr(index, stepID, err.Error())
		}
		if !reflect.DeepEqual(expected, actual) {
			return stepErr(index, stepID, "descrips mismatch")
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
