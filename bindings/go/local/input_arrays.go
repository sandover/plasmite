package local

/*
#include "borrowed.h"
*/
import "C"

import (
	"encoding/binary"
	"math"
	"reflect"
	"unsafe"
)

// A homogeneous scalar sequence needs one compact value-at-visit snapshot,
// rather than a general tagged descriptor for every element. Custom element
// methods keep the ordinary visitor so their addressability and order survive.
func (in *nativeInput) collectScalars(value reflect.Value, parent uintptr, key string, hasKey bool, depth int) (bool, error) {
	element := value.Type().Elem()
	if typeHasMarshalMethod(element) {
		return false, nil
	}
	kind := element.Kind()
	var descriptor uint32
	width := 8
	switch kind {
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64,
		reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64, reflect.Uintptr:
		descriptor = 11
	case reflect.Float32, reflect.Float64:
		descriptor = 12
	case reflect.Bool:
		descriptor = 13
		width = 1
	default:
		return false, nil
	}
	length := value.Len()
	if length > int(^uint(0)>>1)/width {
		return true, invalidArgumentError("scalar array is too large")
	}
	snapshot := make([]byte, length*width)
	if value.Kind() == reflect.Slice && element.Size() == 8 && (kind == reflect.Int || kind == reflect.Int64) {
		// These slices already use native i64 bits. Copy once without rebuilding
		// each integer; the private snapshot still precedes any later hook.
		copy(snapshot, unsafe.Slice((*byte)(value.UnsafePointer()), len(snapshot)))
	} else {
		for i := 0; i < length; i++ {
			item := value.Index(i)
			switch kind {
			case reflect.Bool:
				if item.Bool() {
					snapshot[i] = 1
				}
			case reflect.Float32, reflect.Float64:
				number := item.Float()
				if math.IsInf(number, 0) || math.IsNaN(number) {
					return true, invalidArgumentError("number must be finite")
				}
				binary.NativeEndian.PutUint64(snapshot[i*8:], math.Float64bits(number))
			case reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64, reflect.Uintptr:
				number := item.Uint()
				if number > math.MaxInt64 {
					return true, invalidArgumentError("integer exceeds i64")
				}
				binary.NativeEndian.PutUint64(snapshot[i*8:], number)
			default:
				binary.NativeEndian.PutUint64(snapshot[i*8:], uint64(item.Int()))
			}
		}
	}

	index := in.add(parent, key, hasKey, depth, descriptor)
	in.nodes[index].text = in.bytes(snapshot)
	return true, nil
}
