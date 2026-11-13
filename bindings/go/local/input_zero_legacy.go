//go:build !go1.24

package local

import "reflect"

// Older encoding/json versions ignore this option.
func inputOmitZero(reflect.Type) func(reflect.Value) bool { return nil }
