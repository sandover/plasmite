package udp

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"testing"
	"time"

	"github.com/sandover/plasmite/bindings/go/api"
	"github.com/sandover/plasmite/bindings/go/local"
)

const testPoolSizeBytes uint64 = 1024 * 1024

func TestSDKSendsJSONAndOriginalLite3ThroughServer(t *testing.T) {
	repoRoot := repositoryRoot(t)
	binary := os.Getenv("PLASMITE_BIN")
	if binary == "" {
		binary = filepath.Join(repoRoot, "target", "debug", "plasmite")
	}
	if _, err := os.Stat(binary); err != nil {
		t.Skipf("Plasmite CLI is not built: %s", binary)
	}

	temp := t.TempDir()
	poolDir := filepath.Join(temp, "pools")
	home := filepath.Join(temp, "home")
	if err := os.MkdirAll(poolDir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(home, 0o755); err != nil {
		t.Fatal(err)
	}

	localClient, err := local.NewClient(poolDir)
	if err != nil {
		t.Fatalf("open local client: %v", err)
	}
	t.Cleanup(localClient.Close)
	localPool, err := localClient.CreatePool(local.PoolRefName("udp-sdk"), testPoolSizeBytes)
	if err != nil {
		t.Fatalf("create test pool: %v", err)
	}
	t.Cleanup(localPool.Close)
	original, err := localPool.Append(map[string]any{"origin": "lite3", "value": 7}, []string{"source"})
	if err != nil {
		t.Fatalf("append original message: %v", err)
	}
	originalLite3, err := localPool.GetLite3(original.Seq)
	if err != nil {
		t.Fatalf("read original Lite3: %v", err)
	}

	port, err := reserveUDPPort()
	if err != nil {
		t.Fatalf("reserve UDP port: %v", err)
	}
	ready := filepath.Join(temp, "server-ready")
	var stderr bytes.Buffer
	server := exec.Command(binary,
		"--dir", poolDir,
		"serve",
		"--bind", "127.0.0.1:0",
		"--remote-bind", "127.0.0.1:0",
		"--udp=127.0.0.1:"+port,
		"--udp-follow",
	)
	server.Env = append(os.Environ(),
		"HOME="+home,
		"PLASMITE_ACCESS_HOME="+home,
		"PLASMITE_SERVE_READY_FILE="+ready,
	)
	server.Stderr = &stderr
	if err := server.Start(); err != nil {
		t.Fatalf("start server: %v", err)
	}
	exited := make(chan struct{})
	var serverErr error
	go func() {
		serverErr = server.Wait()
		close(exited)
	}()
	t.Cleanup(func() {
		select {
		case <-exited:
		default:
			_ = server.Process.Kill()
			<-exited
		}
	})

	deadline := time.Now().Add(10 * time.Second)
	for {
		if _, err := os.Stat(ready); err == nil {
			break
		}
		select {
		case <-exited:
			t.Fatalf("server exited before readiness: %v\n%s", serverErr, stderr.String())
		default:
		}
		if time.Now().After(deadline) {
			t.Fatalf("server did not become ready\n%s", stderr.String())
		}
		time.Sleep(20 * time.Millisecond)
	}

	udpClient, err := NewClient("udp://127.0.0.1:" + port)
	if err != nil {
		t.Fatalf("open UDP client: %v", err)
	}
	t.Cleanup(udpClient.Close)
	udpPool, err := udpClient.OpenPool("udp-sdk")
	if err != nil {
		t.Fatalf("open UDP pool: %v", err)
	}
	t.Cleanup(udpPool.Close)
	options := NewFollowOptions()
	options.Start = Sequence(original.Seq + 1)
	follow, err := udpPool.Follow(options)
	if err != nil {
		t.Fatalf("open UDP follow: %v", err)
	}
	t.Cleanup(follow.Close)

	if err := udpPool.SendJSON(map[string]any{"origin": "json", "value": 8}, []string{"sensor"}); err != nil {
		t.Fatalf("send JSON: %v", err)
	}
	jsonFrame := nextFrame(t, follow)
	var jsonData map[string]any
	if err := json.Unmarshal(jsonFrame.Message.Data, &jsonData); err != nil {
		t.Fatalf("decode followed JSON: %v", err)
	}
	if jsonData["origin"] != "json" || jsonData["value"] != float64(8) {
		t.Fatalf("unexpected JSON message: %s", jsonFrame.Message.Data)
	}
	if got := jsonFrame.Message.Tags(); len(got) != 1 || got[0] != "sensor" {
		t.Fatalf("unexpected JSON tags: %#v", got)
	}
	jsonTime, err := localPool.GetLite3(jsonFrame.Seq)
	if err != nil {
		t.Fatalf("read JSON timestamp: %v", err)
	}
	if jsonFrame.TimestampNs != jsonTime.TimestampNs || jsonFrame.TimestampNs <= 1<<53 {
		t.Fatalf("timestamp lost uint64 precision: followed=%d stored=%d", jsonFrame.TimestampNs, jsonTime.TimestampNs)
	}
	storedJSON, err := localPool.Get(jsonFrame.Seq)
	if err != nil {
		t.Fatalf("read stored JSON message: %v", err)
	}
	if !bytes.Equal(storedJSON.Data, jsonFrame.Message.Data) {
		t.Fatalf("followed JSON differs from stored message")
	}

	if err := udpPool.SendLite3(originalLite3.Payload); err != nil {
		t.Fatalf("send Lite3: %v", err)
	}
	lite3Frame := nextFrame(t, follow)
	if !bytes.Equal(lite3Frame.Payload, originalLite3.Payload) {
		t.Fatalf("followed Lite3 bytes differ from submitted bytes")
	}
	if !bytes.Equal(lite3Frame.Message.Data, original.Data) {
		t.Fatalf("followed Lite3 message differs from source message")
	}
	storedLite3, err := localPool.Get(lite3Frame.Seq)
	if err != nil {
		t.Fatalf("read stored Lite3 message: %v", err)
	}
	if !bytes.Equal(storedLite3.Data, original.Data) {
		t.Fatalf("followed Lite3 message differs from stored message")
	}

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	largeStart := (uint64(1) << 53) + 1
	largeFollow, err := udpPool.Follow(FollowOptions{Start: Sequence(largeStart), Live: false})
	if err != nil {
		t.Fatalf("open follow above JavaScript's safe integer range: %v", err)
	}
	defer largeFollow.Close()
	progressEvent, err := largeFollow.Next(ctx)
	if err != nil {
		t.Fatalf("read large-sequence progress: %v", err)
	}
	progress, ok := progressEvent.(*api.UDPProgress)
	if !ok || progress.StartSeq != largeStart {
		t.Fatalf("large sequence changed in binding: got %#v, want %d", progressEvent, largeStart)
	}

	// A canceled wait must return promptly, while a live context still follows
	// progress and messages through the same native handle.
	cancelFollow, err := udpPool.Follow(NewFollowOptions())
	if err != nil {
		t.Fatalf("open cancellation follow: %v", err)
	}
	defer cancelFollow.Close()
	if _, err := cancelFollow.Next(ctx); err != nil {
		t.Fatalf("read opening progress: %v", err)
	}
	ctx, cancelWait := context.WithCancel(context.Background())
	defer cancelWait()
	done := make(chan error, 1)
	go func() {
		_, err := cancelFollow.Next(ctx)
		done <- err
	}()
	time.Sleep(20 * time.Millisecond)
	cancelWait()
	select {
	case err := <-done:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("canceled wait returned %v, want context.Canceled", err)
		}
	case <-time.After(time.Second):
		t.Fatal("canceled wait did not return promptly")
	}
}

func nextFrame(t *testing.T, follow *Follow) *api.UDPFrame {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	for {
		event, err := follow.Next(ctx)
		if err != nil {
			t.Fatalf("wait for UDP frame: %v", err)
		}
		if frame, ok := event.(*api.UDPFrame); ok {
			return frame
		}
	}
}

func reserveUDPPort() (string, error) {
	conn, err := net.ListenUDP("udp", &net.UDPAddr{IP: net.ParseIP("127.0.0.1")})
	if err != nil {
		return "", err
	}
	defer conn.Close()
	address, ok := conn.LocalAddr().(*net.UDPAddr)
	if !ok {
		return "", errors.New("UDP listener has an unexpected address type")
	}
	return strconv.Itoa(address.Port), nil
}

func repositoryRoot(t *testing.T) string {
	t.Helper()
	_, file, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("could not locate test source")
	}
	root := filepath.Clean(filepath.Join(filepath.Dir(file), "../../.."))
	return root
}
