package api

import (
	"path/filepath"
	"testing"
)

func TestExplicitBarePoolPath(t *testing.T) {
	for _, name := range []string{"bare", "space 東京.plasmite"} {
		expected := PoolRef("." + string(filepath.Separator) + name)
		if got := PoolRefPath(name); got != expected {
			t.Fatalf("path %q = %q, want %q", name, got, expected)
		}
		if got := PoolRefName(name); got != PoolRef(name) {
			t.Fatalf("name changed: %q", got)
		}
	}
	for _, path := range []string{"", "./bare", "dir/pool.plasmite"} {
		if got := PoolRefPath(path); got != PoolRef(path) {
			t.Fatalf("path changed: %q", got)
		}
	}
}
