#!/usr/bin/env bash
# Verify a tagged Go module from a fresh project with only the installed SDK.
set -euo pipefail

if [[ $# -ne 2 || ! "$1" =~ ^v2\.[0-9]+\.[0-9]+$ ]]; then
  echo "usage: go_module_smoke.sh v2.X.Y /path/to/sdk" >&2
  exit 2
fi

version="$1"
sdk="$(cd "$2" && pwd)"
test -f "$sdk/lib/pkgconfig/plasmite.pc"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"

module=github.com/sandover/plasmite/bindings/go/v2
export GOWORK=off CGO_ENABLED=1
export PKG_CONFIG_PATH="$sdk/lib/pkgconfig"
case "$(uname -s)" in
  Darwin) export DYLD_LIBRARY_PATH="$sdk/lib" ;;
  Linux) export LD_LIBRARY_PATH="$sdk/lib" ;;
esac

go mod init example.com/plasmite-go-smoke
go get "$module/local@$version"
test "$(go list -m -f '{{.Version}}' "$module")" = "$version"

cat > main.go <<'GO'
package main

import (
	"context"
	"errors"
	"io"
	"log"
	"time"

	"github.com/sandover/plasmite/bindings/go/v2/local"
	_ "github.com/sandover/plasmite/bindings/go/v2/udp"
)

func must[T any](value T, err error) T {
	if err != nil {
		log.Fatal(err)
	}
	return value
}

func main() {
	client := must(local.NewClient("pools"))
	defer client.Close()
	pool := must(client.OpenPool("events", local.Create(true)))
	defer pool.Close()
	sent := must(pool.Feed(map[string]bool{"ok": true}, []string{"smoke"}))

	reader := must(pool.Fetch(local.Since(sent.Seq), local.Tags("smoke")))
	defer reader.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	got := must(reader.Next(ctx))
	if got.Seq != sent.Seq || string(got.Data) != `{"ok":true}` {
		log.Fatalf("unexpected record: %+v", got)
	}
	if _, err := reader.Next(ctx); !errors.Is(err, io.EOF) {
		log.Fatalf("fetch completion: %v", err)
	}
}
GO

go run .
echo "Go $version module install and SDK round trip passed"
