package api

import (
	"context"
	"fmt"
	"math"
	"strconv"
	"strings"
	"time"
)

type OpenConfig struct{ Create bool }
type OpenOption func(*OpenConfig)

func Create(create bool) OpenOption {
	return func(cfg *OpenConfig) { cfg.Create = create }
}

type PoolInfo struct {
	Name     string `json:"name"`
	Path     string `json:"path"`
	FileSize uint64 `json:"file_size"`
	Bounds   struct {
		Oldest *uint64 `json:"oldest"`
		Newest *uint64 `json:"newest"`
	} `json:"bounds"`
}

type Reader interface {
	Next(ctx context.Context) (*Message, error)
	Close()
}

type ReadKind uint32

const (
	ReadDefault ReadKind = iota
	ReadOldest
	ReadSequence
	ReadTime
	ReadAgo
	ReadLast
)

type ReadConfig struct {
	Kind  ReadKind
	Value uint64
	Tags  []string
}

type ReadOption func(*ReadConfig) error

type Instant uint64

func At(timeNS uint64) Instant { return Instant(timeNS) }

func Since(selection any) ReadOption {
	return func(cfg *ReadConfig) error {
		if cfg.Kind != ReadDefault {
			return &Error{Kind: ErrorUsage, Message: "choose one of since and last"}
		}
		kind, value, err := readSince(selection)
		if err != nil {
			return err
		}
		cfg.Kind, cfg.Value = kind, value
		return nil
	}
}

func Last(count uint64) ReadOption {
	return func(cfg *ReadConfig) error {
		if cfg.Kind != ReadDefault {
			return &Error{Kind: ErrorUsage, Message: "choose one of since and last"}
		}
		cfg.Kind, cfg.Value = ReadLast, count
		return nil
	}
}

func Tags(tags ...string) ReadOption {
	return func(cfg *ReadConfig) error {
		cfg.Tags = append(cfg.Tags, tags...)
		return nil
	}
}

func readSince(value any) (ReadKind, uint64, error) {
	switch value := value.(type) {
	case uint64:
		return ReadSequence, value, nil
	case int:
		if value > 0 {
			return ReadSequence, uint64(value), nil
		}
	case Instant:
		return ReadTime, uint64(value), nil
	case time.Time:
		return ReadTime, unixNanoseconds(value), nil
	case time.Duration:
		if value >= 0 {
			return ReadAgo, uint64(value), nil
		}
	case string:
		value = strings.TrimSpace(value)
		if strings.EqualFold(value, "oldest") {
			return ReadOldest, 0, nil
		}
		if duration, ok := parseDurationNS(value); ok {
			return ReadAgo, duration, nil
		}
		if instant, err := time.Parse(time.RFC3339Nano, value); err == nil {
			return ReadTime, unixNanoseconds(instant), nil
		}
	}
	return ReadDefault, 0, &Error{Kind: ErrorUsage,
		Message: fmt.Sprintf("invalid since selection %v", value)}
}

func parseDurationNS(text string) (uint64, bool) {
	unit := strings.IndexFunc(text, func(c rune) bool { return c < '0' || c > '9' })
	if unit <= 0 {
		return 0, false
	}
	scale := map[string]uint64{"ns": 1, "us": 1000, "ms": 1000000, "s": 1000000000,
		"m": uint64(time.Minute), "h": uint64(time.Hour), "d": uint64(24 * time.Hour)}
	multiplier, ok := scale[strings.ToLower(text[unit:])]
	count, err := strconv.ParseUint(text[:unit], 10, 64)
	if !ok || err != nil {
		return 0, false
	}
	if count > math.MaxUint64/multiplier {
		return math.MaxUint64, true
	}
	return count * multiplier, true
}

func unixNanoseconds(instant time.Time) uint64 {
	seconds := instant.Unix()
	if seconds < 0 {
		return 0
	}
	nanos := uint64(instant.Nanosecond())
	if uint64(seconds) > (math.MaxUint64-nanos)/1_000_000_000 {
		return math.MaxUint64
	}
	return uint64(seconds)*1_000_000_000 + nanos
}
