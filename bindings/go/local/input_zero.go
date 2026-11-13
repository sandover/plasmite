//go:build go1.24

package local

import "reflect"

type inputIsZero interface{ IsZero() bool }

var inputIsZeroType = reflect.TypeOf((*inputIsZero)(nil)).Elem()

func inputOmitZero(t reflect.Type) func(reflect.Value) bool {
	switch {
	case t.Kind() == reflect.Interface && t.Implements(inputIsZeroType):
		return func(value reflect.Value) bool {
			return value.IsNil() || (value.Elem().Kind() == reflect.Pointer && value.Elem().IsNil()) || value.Interface().(inputIsZero).IsZero()
		}
	case t.Kind() == reflect.Pointer && t.Implements(inputIsZeroType):
		return func(value reflect.Value) bool { return value.IsNil() || value.Interface().(inputIsZero).IsZero() }
	case t.Implements(inputIsZeroType):
		return func(value reflect.Value) bool { return value.Interface().(inputIsZero).IsZero() }
	case reflect.PointerTo(t).Implements(inputIsZeroType):
		return func(value reflect.Value) bool {
			if !value.CanAddr() {
				copy := reflect.New(t).Elem()
				copy.Set(value)
				value = copy
			}
			return value.Addr().Interface().(inputIsZero).IsZero()
		}
	default:
		return func(value reflect.Value) bool { return value.IsZero() }
	}
}
