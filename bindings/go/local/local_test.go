/*
Purpose: Validate Go binding behaviors for streaming, payloads, and lifecycle.
Key Exports: None (package tests).
Role: Exercise tail cancellation/timeout and large payload handling.
Invariants: Requires libplasmite build output in library search path.
Notes: Uses temporary directories; avoids global state.
*/
package local

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"os"
	"os/user"
	"path/filepath"
	"reflect"
	"runtime"
	"strings"
	"testing"
	"time"

	"github.com/sandover/plasmite/bindings/go/v2/api"
)

const testPoolSizeBytes uint64 = 1024 * 1024

func newTestClient(t *testing.T) *Client {
	t.Helper()
	poolDir := filepath.Join(t.TempDir(), "pools")
	if err := os.MkdirAll(poolDir, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	client, err := NewClient(poolDir)
	if err != nil {
		t.Fatalf("client: %v", err)
	}
	t.Cleanup(func() {
		client.Close()
	})
	return client
}

func newTestPool(t *testing.T, client *Client, name string) *Pool {
	t.Helper()
	pool, err := client.CreatePool(PoolRefName(name), testPoolSizeBytes)
	if err != nil {
		t.Fatalf("create: %v", err)
	}
	t.Cleanup(func() {
		pool.Close()
	})
	return pool
}

func fillPast(t *testing.T, pool *Pool, seq uint64) {
	t.Helper()
	for i := 0; i < 10_000; i++ {
		if _, err := pool.Feed(
			map[string]any{"padding": strings.Repeat("x", 64*1024), "i": i},
			nil,
			WithDurability(DurabilityFast),
		); err != nil {
			t.Fatalf("fill pool: %v", err)
		}
		if _, err := getTestMessage(pool, seq); err != nil {
			var plasmiteErr *Error
			if errors.As(err, &plasmiteErr) && plasmiteErr.Kind == ErrorRetentionGap {
				return
			}
			t.Fatalf("check retained sequence: %v", err)
		}
	}
	t.Fatalf("sequence %d remained retained after filling the pool", seq)
}

func TestPoolLookupErrorsDistinguishMissingSequenceAndCorruptFile(t *testing.T) {
	client := newTestClient(t)
	poolPath := filepath.Join(t.TempDir(), "damaged.plasmite")
	ref := PoolRefPath(poolPath)

	assertError := func(err error, kind ErrorKind, path string, seq *uint64) {
		t.Helper()
		var got *Error
		if !errors.As(err, &got) || got.Kind != kind || got.Path != path ||
			(seq == nil && got.Seq != nil) || (seq != nil && (got.Seq == nil || *got.Seq != *seq)) {
			t.Fatalf("expected kind %v, path %q and sequence %v, got %#v", kind, path, seq, got)
		}
	}

	_, err := client.OpenPool(ref)
	assertError(err, ErrorNotFound, poolPath, nil)
	pool, err := client.CreatePool(ref, testPoolSizeBytes)
	if err != nil {
		t.Fatal(err)
	}
	_, err = getTestMessage(pool, 1)
	pool.Close()
	assertError(err, ErrorNotFound, "", uint64Ptr(1))

	if err := os.Truncate(poolPath, 4); err != nil {
		t.Fatal(err)
	}
	_, err = client.OpenPool(ref)
	assertError(err, ErrorCorrupt, poolPath, nil)
}

func TestAppendGetLargePayload(t *testing.T) {
	client := newTestClient(t)
	pool := newTestPool(t, client, "big")

	payload := strings.Repeat("x", 64*1024)
	data := map[string]any{"blob": payload}
	tags := []string{"alpha", "beta", "gamma"}
	msg, err := pool.Feed(data, tags, WithDurability(DurabilityFast))
	if err != nil {
		t.Fatalf("append: %v", err)
	}
	decoded, err := decodeObject(msg.Data())
	if err != nil {
		t.Fatalf("decode append: %v", err)
	}
	if decoded["blob"] != payload {
		t.Fatalf("append payload mismatch")
	}
	if len(msg.Tags()) != len(tags) {
		t.Fatalf("tags mismatch")
	}

	getMsg, err := getTestMessage(pool, msg.Seq())
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	decodedGet, err := decodeObject(getMsg.Data())
	if err != nil {
		t.Fatalf("decode get: %v", err)
	}
	if decodedGet["blob"] != payload {
		t.Fatalf("get payload mismatch")
	}
}

func TestClientPoolCreateThenReopen(t *testing.T) {
	client := newTestClient(t)

	first, err := client.OpenPool(PoolRefName("work"), Create(true))
	if err != nil {
		t.Fatalf("pool create/open first: %v", err)
	}
	defer first.Close()

	second, err := client.OpenPool(PoolRefName("work"), Create(true))
	if err != nil {
		t.Fatalf("pool create/open second: %v", err)
	}
	defer second.Close()

	msg, err := first.Feed(map[string]any{"kind": "created"}, []string{"alpha"}, WithDurability(DurabilityFast))
	if err != nil {
		t.Fatalf("append: %v", err)
	}

	getMsg, err := getTestMessage(second, msg.Seq())
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	decoded, err := decodeObject(getMsg.Data())
	if err != nil {
		t.Fatalf("decode get: %v", err)
	}
	if decoded["kind"] != "created" {
		t.Fatalf("expected created kind")
	}
}

func TestFollowLite3RetentionGapPolicy(t *testing.T) {
	client := newTestClient(t)
	pool := newTestPool(t, client, "lite3-gap")
	first, err := pool.Feed(map[string]any{"kind": "first"}, nil)
	if err != nil {
		t.Fatalf("append first: %v", err)
	}
	fillPast(t, pool, first.Seq())

	_, err = pool.followLite3(Since(first.Seq()), Tags("never"))
	var plasmiteErr *Error
	if !errors.As(err, &plasmiteErr) || plasmiteErr.Kind != ErrorRetentionGap {
		t.Fatalf("expected retention gap, got %T: %v", err, err)
	}
	if plasmiteErr.Missing != first.Seq() || plasmiteErr.NextSeq <= first.Seq() {
		t.Fatalf("expected gap sequence %d, got %#v", first.Seq(), plasmiteErr)
	}
}

func TestOpenFollowReportsFirstMissingSequenceAfterOverwrite(t *testing.T) {
	client := newTestClient(t)
	pool := newTestPool(t, client, "slow-reader")
	seed, err := pool.Feed(map[string]any{"kind": "seed"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	stream, err := pool.Follow(Since(seed.Seq()))
	if err != nil {
		t.Fatal(err)
	}
	defer stream.Close()
	ctx, stop := context.WithTimeout(context.Background(), 2*time.Second)
	defer stop()
	message, err := stream.Next(ctx)
	if err != nil || message.Seq() != seed.Seq() {
		t.Fatalf("read seed: %v (%v)", message, err)
	}

	firstMissing := seed.Seq() + 1
	if _, err := pool.Feed(map[string]any{"kind": "lost"}, nil); err != nil {
		t.Fatal(err)
	}
	fillPast(t, pool, firstMissing)
	_, err = stream.Next(ctx)
	var gap *Error
	if !errors.As(err, &gap) || gap.Kind != ErrorRetentionGap || gap.Missing != firstMissing || gap.NextSeq <= firstMissing {
		t.Fatalf("expected gap at %d, got %#v", firstMissing, err)
	}
	if _, err := stream.Next(ctx); !errors.As(err, &gap) || gap.Missing != firstMissing {
		t.Fatalf("expected the reader to keep reporting the gap, got %v", err)
	}
}

func TestCloseIdempotent(t *testing.T) {
	client := newTestClient(t)
	pool := newTestPool(t, client, "close")
	pool.Close()
	pool.Close()

	_, err := pool.Feed(map[string]any{"n": 1}, nil, WithDurability(DurabilityFast))
	if err == nil {
		t.Fatalf("expected error on closed pool")
	}
	if !isUsage(err) {
		t.Fatalf("expected ErrClosed, got %v", err)
	}

	client.Close()
	client.Close()
	_, err = client.CreatePool(PoolRefName("oops"), testPoolSizeBytes)
	if err == nil {
		t.Fatalf("expected error on closed client")
	}
	if !isUsage(err) {
		t.Fatalf("expected ErrClosed, got %v", err)
	}
}

func TestArgumentErrorsUseSentinel(t *testing.T) {
	if _, err := NewClient(""); !isUsage(err) {
		t.Fatalf("expected ErrInvalidArgument for empty poolDir, got %v", err)
	}

	client := newTestClient(t)

	if _, err := client.CreatePool(PoolRefName(""), testPoolSizeBytes); !isUsage(err) {
		t.Fatalf("expected ErrInvalidArgument for empty pool ref, got %v", err)
	}

	pool := newTestPool(t, client, "args")

	if _, err := pool.Feed(JSON(nil), nil); !isUsage(err) {
		t.Fatalf("expected ErrInvalidArgument for empty payload, got %v", err)
	}
}

func TestLite3AppendGetTail(t *testing.T) {
	client := newTestClient(t)
	pool := newTestPool(t, client, "lite3")

	msg, err := pool.Feed(map[string]any{"x": 1}, []string{"alpha"}, WithDurability(DurabilityFast))
	if err != nil {
		t.Fatalf("append json: %v", err)
	}

	seedFrame, err := pool.getLite3(msg.Seq())
	if err != nil {
		t.Fatalf("get lite3: %v", err)
	}
	if len(seedFrame.Payload) == 0 {
		t.Fatalf("expected lite3 payload")
	}

	seq2, err := feedLite3Sequence(pool, seedFrame.Payload, DurabilityFast)
	if err != nil {
		t.Fatalf("append lite3: %v", err)
	}
	frame2, err := pool.getLite3(seq2)
	if err != nil {
		t.Fatalf("get lite3 (seq2): %v", err)
	}
	if !bytes.Equal(seedFrame.Payload, frame2.Payload) {
		t.Fatalf("lite3 payload mismatch")
	}

	stream, err := pool.fetchLite3(Since(seq2))
	if err != nil {
		t.Fatalf("open lite3 stream: %v", err)
	}
	defer stream.Close()
	streamFrame, err := stream.Next(context.Background())
	if err != nil {
		t.Fatalf("stream next: %v", err)
	}
	if streamFrame.Seq != seq2 {
		t.Fatalf("stream seq mismatch")
	}
	if !bytes.Equal(seedFrame.Payload, streamFrame.Payload) {
		t.Fatalf("stream payload mismatch")
	}
	if _, err := stream.Next(context.Background()); !errors.Is(err, io.EOF) {
		t.Fatalf("expected eof, got %v", err)
	}

}

func TestLite3AppendRejectsInvalidPayload(t *testing.T) {
	client := newTestClient(t)
	pool := newTestPool(t, client, "lite3-bad")

	_, err := feedLite3Sequence(pool, []byte{0x01, 0x02, 0x03}, DurabilityFast)
	if err == nil {
		t.Fatalf("expected error")
	}
	var perr *Error
	if !errors.As(err, &perr) {
		t.Fatalf("expected plasmite error")
	}
	if perr.Kind != ErrorCorrupt && perr.Kind != ErrorUsage {
		t.Fatalf("unexpected error kind: %v", perr.Kind)
	}
}

func uint64Ptr(val uint64) *uint64 {
	return &val
}

func feedLite3Sequence(pool *Pool, payload []byte, durability Durability) (uint64, error) {
	receipt, err := pool.feedLite3Receipt(payload, WithDurability(durability))
	if err != nil {
		return 0, err
	}
	return receipt.Seq, nil
}

func getTestMessage(pool *Pool, seq uint64) (*api.Message, error) {
	reader, err := pool.Fetch(Since(seq))
	if err != nil {
		return nil, err
	}
	defer reader.Close()
	message, err := reader.Next(context.Background())
	if err == io.EOF {
		return nil, &Error{Kind: ErrorNotFound, Message: "message not found", Seq: &seq}
	}
	return message, err
}

func decodeObject(raw json.RawMessage) (map[string]any, error) {
	var out map[string]any
	if err := json.Unmarshal(raw, &out); err != nil {
		return nil, err
	}
	return out, nil
}

func TestDefaultPoolDirectory(t *testing.T) {
	account, err := user.Current()
	if err != nil {
		t.Fatal(err)
	}
	key := "HOME"
	if runtime.GOOS == "windows" {
		key = "USERPROFILE"
	}
	for _, value := range []string{"", "relative", filepath.Join(t.TempDir(), "home space 東京")} {
		t.Setenv(key, value)
		directory, err := DefaultPoolDir()
		if err != nil {
			t.Fatal(err)
		}
		home := value
		if !filepath.IsAbs(home) {
			home = account.HomeDir
		}
		if want := filepath.Join(home, ".plasmite", "pools"); directory != want {
			t.Fatalf("got %q, want %q", directory, want)
		}
	}
}

func TestNumericFeedReceipts(t *testing.T) {
	pool := newTestPool(t, newTestClient(t), "identities")
	value := map[string]any{"text": "hello"}
	first, err := pool.feedNativeReceipt(value, []string{"tag"}, api.WithDurability(api.DurabilityFlush))
	if err != nil {
		t.Fatal(err)
	}
	stored, err := pool.getLite3(first.Seq)
	if err != nil {
		t.Fatal(err)
	}
	if first.Seq != 1 || first.Time != stored.Time || first.Time <= 1<<53 {
		t.Fatalf("receipt mismatch: %+v / %+v", first, stored)
	}
	value["text"] = "changed"
	message, err := getTestMessage(pool, first.Seq)
	if err != nil {
		t.Fatal(err)
	}
	if string(message.Data()) != `{"text":"hello"}` {
		t.Fatalf("stored data: %s", message.Data())
	}
	second, err := pool.feedLite3Receipt(stored.Payload)
	if err != nil {
		t.Fatal(err)
	}
	frame, err := pool.getLite3(second.Seq)
	if err != nil {
		t.Fatal(err)
	}
	if second.Seq != 2 || second.Time != frame.Time {
		t.Fatalf("binary identity: %+v", second)
	}
	if _, err := pool.feedJSONReceipt([]byte("bad"), nil); err == nil {
		t.Fatal("accepted bad JSON")
	}
	if _, err := pool.feedLite3Receipt([]byte("bad")); err == nil {
		t.Fatal("accepted bad binary")
	}
	third, err := pool.feedNativeReceipt(map[string]any{}, nil)
	if err != nil || third.Seq != 3 {
		t.Fatalf("advanced on rejected append: %+v / %v", third, err)
	}
}

func TestV2Listener(t *testing.T) {
	client := newTestClient(t)
	pool, err := client.OpenPool("listener", Create(true))
	if err != nil {
		t.Fatal(err)
	}
	defer pool.Close()
	old, err := pool.Feed(map[string]int{"n": 1}, []string{"keep\x00canvas"})
	if err != nil {
		t.Fatal(err)
	}
	reader, err := pool.Follow(Tags("keep\x00canvas"))
	if err != nil {
		t.Fatal(err)
	}
	defer reader.Close()
	if _, err := pool.Feed(map[string]int{"n": 2}, nil); err != nil {
		t.Fatal(err)
	}
	newest, err := pool.Feed(old, nil)
	if err != nil {
		t.Fatal(err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	got, err := reader.Next(ctx)
	if err != nil || got.Seq() != newest.Seq() || got.Time() != newest.Time() {
		t.Fatalf("live read: %v, %v", got, err)
	}
	if tags := got.Tags(); len(tags) != 1 || tags[0] != "keep\x00canvas" {
		t.Fatalf("read changed tag bytes: %q", tags)
	}
	info, err := pool.Info()
	if err != nil || info.Bounds.Oldest == nil || *info.Bounds.Oldest != old.Seq() {
		t.Fatalf("info: %v, %v", info, err)
	}
	fetch, err := pool.Fetch(Since(newest.Seq()), Tags("keep\x00canvas"))
	if err != nil {
		t.Fatal(err)
	}
	defer fetch.Close()
	got, err = fetch.Next(ctx)
	if err != nil || got.Seq() != newest.Seq() {
		t.Fatalf("resume: %v, %v", got, err)
	}
	if _, err = fetch.Next(ctx); !errors.Is(err, io.EOF) {
		t.Fatalf("finite read ended with %v", err)
	}
}

func TestSharedV2ReadCases(t *testing.T) {
	type seed struct {
		Data json.RawMessage
		Tags []string
	}
	type scenario struct {
		Name      string
		Since     json.RawMessage
		Last      *uint64
		Tags      []string
		Expect    []uint64
		Error     string
		AfterOpen []seed `json:"after_open"`
	}
	var cases struct {
		Seed     []seed
		Fetch    []scenario
		Single   []scenario `json:"single_fetch"`
		Follow   []scenario
		Snapshot scenario `json:"fetch_snapshot"`
	}
	fixture, err := os.ReadFile("../../../tests/fixtures/conformance/reads.json")
	if err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(fixture, &cases); err != nil {
		t.Fatal(err)
	}
	cases.Fetch = append(cases.Fetch, cases.Snapshot)
	for _, group := range []struct {
		live, single bool
		reads        []scenario
	}{
		{reads: cases.Fetch},
		{single: true, reads: cases.Single},
		{live: true, reads: cases.Follow},
	} {
		for _, example := range group.reads {
			t.Run(example.Name, func(t *testing.T) {
				pool := newTestPool(t, newTestClient(t), "events")
				write := func(records []seed) {
					for _, message := range records {
						if _, err := pool.Feed(message.Data, message.Tags); err != nil {
							t.Fatal(err)
						}
					}
				}
				write(cases.Seed)
				options := []api.ReadOption{Tags(example.Tags...)}
				if len(example.Since) != 0 {
					var sequence uint64
					if err := json.Unmarshal(example.Since, &sequence); err == nil {
						options = append(options, Since(sequence))
					} else {
						var selection string
						if err := json.Unmarshal(example.Since, &selection); err != nil {
							t.Fatal(err)
						}
						options = append(options, Since(selection))
					}
				}
				if example.Last != nil {
					options = append(options, Last(*example.Last))
				}
				open := pool.Fetch
				if group.live {
					open = pool.Follow
				}
				reader, err := open(options...)
				if example.Error != "" {
					var kind *Error
					if !errors.As(err, &kind) || kind.Kind != ErrorUsage || example.Error != "Usage" {
						t.Fatalf("wanted %s, got %v", example.Error, err)
					}
					return
				}
				if err != nil {
					t.Fatal(err)
				}
				defer reader.Close()
				write(example.AfterOpen)
				ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
				defer cancel()
				for _, want := range example.Expect {
					got, err := reader.Next(ctx)
					if err != nil || got.Seq() != want {
						t.Fatalf("wanted sequence %d, got %v and %v", want, got, err)
					}
				}
				if !group.live && (!group.single || len(example.Expect) == 0) {
					_, err := reader.Next(ctx)
					if !errors.Is(err, io.EOF) {
						t.Fatalf("expected end of history, got %v", err)
					}
				}
			})
		}
	}
}

func TestFeedForwardsAllMessageRepresentations(t *testing.T) {
	pool := newTestPool(t, newTestClient(t), "forward")
	original, err := pool.Feed(JSON(`{"n":1}`), []string{"tag"})
	if err != nil {
		t.Fatal(err)
	}
	for _, source := range []any{Lite3(original.Raw()), *original, original} {
		forwarded, err := pool.Feed(source, nil)
		if err != nil || string(forwarded.Data()) != string(original.Data()) || !reflect.DeepEqual(forwarded.Tags(), []string{"tag"}) {
			t.Fatalf("forward %T: %#v, %v", source, forwarded, err)
		}
		if _, err := pool.Feed(source, []string{}); !isUsage(err) {
			t.Fatalf("expected Usage for a tag override, got %v", err)
		}
	}
}

func isUsage(err error) bool {
	var typed *api.Error
	return errors.As(err, &typed) && typed.Kind == api.ErrorUsage
}

func TestPoolDescriptionCanBeSetReadAndCleared(t *testing.T) {
	client := newTestClient(t)
	created, err := client.CreatePool("description", 65536, WithDescription(PoolDescription{
		About: "Room readings", Example: JSON(`{"temp_c":22}`),
	}))
	if err != nil {
		t.Fatal(err)
	}
	defer created.Close()
	info, err := created.Info()
	if err != nil || info.About != "Room readings" || string(info.Example) != `{"temp_c":22}` {
		t.Fatalf("description: %+v, %v", info, err)
	}
	if _, err := created.Feed(map[string]any{"temp_c": 23}, nil); err != nil {
		t.Fatal(err)
	}
	list, err := client.ListPools()
	if err != nil || len(list) != 1 || list[0].About != info.About {
		t.Fatalf("list description: %+v, %v", list, err)
	}
	if err := created.SetDescription(PoolDescription{}); err != nil {
		t.Fatal(err)
	}
	info, err = created.Info()
	if err != nil || info.About != "" || info.Example != nil {
		t.Fatalf("description after clear: %+v, %v", info, err)
	}
}

func TestHTTPSPoolReference(t *testing.T) {
	origin := os.Getenv("PLASMITE_TEST_HTTPS_ORIGIN")
	if origin == "" {
		t.Skip("run the HTTPS binding contract smoke")
	}
	client := newTestClient(t)
	ref := PoolRefURI(origin + "/go")
	created, err := client.CreatePool(ref, 65536, WithDescription(PoolDescription{
		About: "HTTPS test messages", Example: JSON(`{"from":"go"}`),
	}))
	if err != nil {
		t.Fatal(err)
	}
	defer created.Close()
	pool, err := client.OpenPool(ref)
	if err != nil {
		t.Fatal(err)
	}
	defer pool.Close()
	info, err := pool.Info()
	if err != nil || info.About != "HTTPS test messages" || string(info.Example) != `{"from":"go"}` {
		t.Fatalf("HTTPS description: %+v, %v", info, err)
	}
	if err := pool.SetDescription(PoolDescription{About: "Updated remotely"}); err != nil {
		t.Fatal(err)
	}
	info, err = pool.Info()
	if err != nil || info.About != "Updated remotely" || info.Example != nil {
		t.Fatalf("updated HTTPS description: %+v, %v", info, err)
	}
	live, err := pool.Follow(Tags("test"))
	if err != nil {
		t.Fatal(err)
	}
	defer live.Close()
	short, expire := context.WithTimeout(context.Background(), 10*time.Millisecond)
	defer expire()
	if _, err := live.Next(short); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("idle wait returned %v", err)
	}
	first, err := created.Feed(map[string]any{"from": "go"}, []string{"test"})
	if err != nil {
		t.Fatal(err)
	}
	if first.Sender() == "" {
		t.Fatal("HTTPS feed returned a message without its authenticated sender")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	message, err := live.Next(ctx)
	if err != nil || message.Seq() != first.Seq() {
		t.Fatalf("follow: %#v, %v", message, err)
	}
	if first.Sender() != message.Sender() || !bytes.Equal(first.Raw(), message.Raw()) {
		t.Fatal("HTTPS feed and follow returned different message encodings")
	}
	reader, err := pool.Fetch(Last(1))
	if err != nil {
		t.Fatal(err)
	}
	defer reader.Close()
	message, err = reader.Next(ctx)
	if err != nil || string(message.Data()) != `{"from":"go"}` {
		t.Fatalf("fetch: %#v, %v", message, err)
	}
	if _, err := reader.Next(ctx); err != io.EOF {
		t.Fatalf("finished fetch returned %v", err)
	}
	destination := newTestPool(t, client, "forwarded")
	local, err := destination.Feed(message, nil)
	if err != nil {
		t.Fatal(err)
	}
	if local.Sender() != "" || !reflect.DeepEqual(local.Tags(), message.Tags()) || !bytes.Equal(local.Data(), message.Data()) {
		t.Fatalf("local forward returned sender %q, tags %v, data %s", local.Sender(), local.Tags(), local.Data())
	}
}

func TestRejectsWrongABIMajor(t *testing.T) {
	if os.Getenv("PLASMITE_WRONG_ABI") == "" {
		t.Skip("run the native binding contract smoke")
	}
	_, err := NewClient(t.TempDir())
	if !isUsage(err) || !strings.Contains(err.Error(), "requires ABI major 2; found 1") {
		t.Fatalf("expected clear ABI version error, got %v", err)
	}
}

func TestLocalIdentifiersRejectEmbeddedNUL(t *testing.T) {
	dir := t.TempDir()
	if client, err := NewClient(dir + "\x00other"); !isUsage(err) {
		client.Close()
		t.Fatalf("NewClient: %v", err)
	}
	client := newTestClient(t)
	newTestPool(t, client, "events")
	invalid := PoolRefName("events\x00other")
	if pool, err := client.OpenPool(invalid); !isUsage(err) {
		pool.Close()
		t.Fatalf("OpenPool: %v", err)
	}
	if pool, err := client.CreatePool(PoolRefName("new\x00other"), testPoolSizeBytes); !isUsage(err) {
		pool.Close()
		t.Fatalf("CreatePool: %v", err)
	}
	if err := client.DeletePool(invalid); !isUsage(err) {
		t.Fatalf("DeletePool: %v", err)
	}
	pools, err := client.ListPools()
	if err != nil || len(pools) != 1 || filepath.Base(pools[0].Path) != "events.plasmite" {
		t.Fatalf("pools changed: %+v, %v", pools, err)
	}
}
