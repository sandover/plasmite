package api

import "testing"

func TestSinceAcceptsPositiveIntegers(t *testing.T) {
	type sequence uint64
	for _, value := range []any{int(3), int8(3), int16(3), int32(3), int64(3), uint(3), uint8(3), uint16(3), uint32(3), uint64(3), uintptr(3), sequence(3)} {
		var cfg ReadConfig
		if err := Since(value)(&cfg); err != nil || cfg.Kind != ReadSequence || cfg.Value != 3 {
			t.Fatalf("Since(%T): %#v, %v", value, cfg, err)
		}
	}
	for _, value := range []any{int(0), uint64(0), int64(-1)} {
		var cfg ReadConfig
		if err := Since(value)(&cfg); err == nil {
			t.Fatalf("Since(%T(%v)) accepted an invalid sequence", value, value)
		}
	}
}
