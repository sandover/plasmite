package local

/*
#include "borrowed.h"
*/
import "C"

import (
	"encoding"
	"encoding/json"
	"fmt"
	"math"
	"reflect"
	"runtime"
	"sort"
	"strconv"
	"strings"
	"sync"
	"unicode"
	"unicode/utf8"
	"unsafe"
)

const inputJSON = 9
const inputSource = 10

var marshalType = reflect.TypeOf((*json.Marshaler)(nil)).Elem()
var textMarshalType = reflect.TypeOf((*encoding.TextMarshaler)(nil)).Elem()
var sourceValueType = reflect.TypeOf(Value{})
var stringAnyMapType = reflect.TypeOf(map[string]any(nil))

// All referenced Go allocations remain pinned only through the native call.
// Mutable bytes and JSON hook output receive private snapshots at encounter.
type nativeInput struct {
	pin   runtime.Pinner
	nodes []C.plsm_go_input_node_t
	tags  []C.plsm_go_input_span_t
}

func (in *nativeInput) bytes(data []byte) C.plsm_go_input_span_t {
	if len(data) == 0 {
		return C.plsm_go_input_span_t{}
	}
	pointer := unsafe.SliceData(data)
	in.pin.Pin(pointer)
	return C.plsm_go_input_span_t{data: (*C.uint8_t)(unsafe.Pointer(pointer)), len: C.size_t(len(data))}
}
func normalizeUTF8(text string) string {
	if utf8.ValidString(text) {
		return text
	}
	// Go's string encoder replaces each invalid byte, not an entire bad run.
	return string([]rune(text))
}
func (in *nativeInput) text(text string) C.plsm_go_input_span_t {
	text = normalizeUTF8(text)
	if len(text) == 0 {
		return C.plsm_go_input_span_t{}
	}
	pointer := unsafe.StringData(text)
	in.pin.Pin(pointer)
	return C.plsm_go_input_span_t{data: (*C.uint8_t)(unsafe.Pointer(pointer)), len: C.size_t(len(text))}
}
func (in *nativeInput) tagValues(tags []string) error {
	in.tags = make([]C.plsm_go_input_span_t, len(tags))
	for i, tag := range tags {
		if !utf8.ValidString(tag) {
			return &Error{Kind: ErrorUsage, Message: "tag is not UTF-8"}
		}
		in.tags[i] = in.text(tag)
	}
	return nil
}

func (in *nativeInput) add(parent uintptr, key string, hasKey bool, depth int, kind uint32) int {
	flags := uint32(depth) << 8
	if hasKey {
		flags |= 1
	}
	node := C.plsm_go_input_node_t{parent: C.size_t(parent), kind: C.uint32_t(kind), flags: C.uint32_t(flags)}
	if hasKey {
		node.key = in.text(key)
	}
	in.nodes = append(in.nodes, node)
	return len(in.nodes) - 1
}
func marshalValue(value reflect.Value) (any, bool) {
	if value.CanAddr() && value.Addr().Type().Implements(marshalType) {
		return value.Addr().Interface(), true
	}
	if value.Type().Implements(marshalType) {
		return value.Interface(), true
	}
	if value.CanAddr() && value.Addr().Type().Implements(textMarshalType) {
		return value.Addr().Interface(), true
	}
	if value.Type().Implements(textMarshalType) {
		return value.Interface(), true
	}
	return nil, false
}
func (in *nativeInput) json(value any, parent uintptr, key string, hasKey bool, depth int) error {
	encoded, err := json.Marshal(value)
	if err != nil {
		return err
	}
	// Marshal returns independent bytes, even when a hook reuses its own buffer.
	index := in.add(parent, key, hasKey, depth, inputJSON)
	in.nodes[index].text = in.bytes(encoded)
	return nil
}
func (in *nativeInput) collect(value reflect.Value, parent uintptr, key string, hasKey bool, depth int) error {
	if !value.IsValid() {
		in.add(parent, key, hasKey, depth, 1)
		return nil
	}
	for value.Kind() == reflect.Interface {
		if value.IsNil() {
			in.add(parent, key, hasKey, depth, 1)
			return nil
		}
		value = value.Elem()
	}
	if value.Kind() == reflect.Pointer && value.IsNil() {
		in.add(parent, key, hasKey, depth, 1)
		return nil
	}
	if value.Type() == sourceValueType {
		source := value.Interface().(Value)
		if source.owner == nil || source.kind == KindMissing {
			return invalidArgumentError("cannot append a missing value")
		}
		index := in.add(parent, key, hasKey, depth, inputSource)
		in.nodes[index].text = in.bytes(source.owner.encoded)
		in.nodes[index].integer = C.int64_t(source.node)
		return nil
	}
	if custom, ok := marshalValue(value); ok {
		return in.json(custom, parent, key, hasKey, depth)
	}
	if value.Type() == jsonNumberType {
		return in.json(value.Interface(), parent, key, hasKey, depth)
	}
	if value.Kind() == reflect.Pointer {
		return in.collect(value.Elem(), parent, key, hasKey, depth)
	}
	switch value.Kind() {
	case reflect.Bool:
		index := in.add(parent, key, hasKey, depth, 2)
		if value.Bool() {
			in.nodes[index].integer = 1
		}
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64:
		index := in.add(parent, key, hasKey, depth, 3)
		in.nodes[index].integer = C.int64_t(value.Int())
	case reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64, reflect.Uintptr:
		number := value.Uint()
		if number > math.MaxInt64 {
			return &Error{Kind: ErrorUsage, Message: "integer exceeds i64"}
		}
		index := in.add(parent, key, hasKey, depth, 3)
		in.nodes[index].integer = C.int64_t(number)
	case reflect.Float32, reflect.Float64:
		number := value.Float()
		if math.IsNaN(number) || math.IsInf(number, 0) {
			return &Error{Kind: ErrorUsage, Message: "number must be finite"}
		}
		index := in.add(parent, key, hasKey, depth, 4)
		in.nodes[index].real = C.double(number)
	case reflect.String:
		index := in.add(parent, key, hasKey, depth, 5)
		in.nodes[index].text = in.text(value.String())
	case reflect.Slice:
		if value.IsNil() {
			in.add(parent, key, hasKey, depth, 1)
			return nil
		}
		if value.Type().Elem().Kind() == reflect.Uint8 && !typeHasMarshalMethod(value.Type().Elem()) {
			index := in.add(parent, key, hasKey, depth, 6)
			// A later hook can mutate the original slice, so pin a snapshot.
			data := append([]byte{}, value.Bytes()...)
			in.nodes[index].text = in.bytes(data)
			return nil
		}
		fallthrough
	case reflect.Array:
		if depth >= 32 {
			return &Error{Kind: ErrorUsage, Message: "container nesting exceeds limit"}
		}
		if packed, err := in.collectScalars(value, parent, key, hasKey, depth); packed {
			return err
		}
		index := in.add(parent, key, hasKey, depth, 8)
		length := value.Len()
		for i := 0; i < length; i++ {
			if err := in.collect(value.Index(i), uintptr(index), "", false, depth+1); err != nil {
				return err
			}
		}
	case reflect.Map:
		if value.IsNil() {
			in.add(parent, key, hasKey, depth, 1)
			return nil
		}
		if depth >= 32 {
			return &Error{Kind: ErrorUsage, Message: "container nesting exceeds limit"}
		}
		keyType := value.Type().Key()
		switch keyType.Kind() {
		case reflect.String, reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64, reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64, reflect.Uintptr:
		default:
			if !keyType.Implements(textMarshalType) {
				return &json.UnsupportedTypeError{Type: value.Type()}
			}
		}
		index := in.add(parent, key, hasKey, depth, 7)
		if value.Type() == stringAnyMapType {
			type item struct {
				key   string
				value any
			}
			object := value.Interface().(map[string]any)
			entries := make([]item, 0, len(object))
			for key, value := range object {
				entries = append(entries, item{key, value})
			}
			sort.Slice(entries, func(i, j int) bool { return entries[i].key < entries[j].key })
			for _, entry := range entries {
				if err := in.collect(reflect.ValueOf(entry.value), uintptr(index), entry.key, true, depth+1); err != nil {
					return err
				}
			}
			return nil
		}
		type entry struct {
			key   string
			value reflect.Value
		}
		entries := make([]entry, 0, value.Len())
		iterator := value.MapRange()
		// encoding/json snapshots map values before visiting callbacks and sorts
		// resolved keys. Preserve that order rather than reading the map later.
		for iterator.Next() {
			name, err := inputMapKey(iterator.Key())
			if err != nil {
				return fmt.Errorf("json: encoding error for type %q: %q", value.Type().String(), err.Error())
			}
			entries = append(entries, entry{key: name, value: iterator.Value()})
		}
		sort.Slice(entries, func(i, j int) bool { return entries[i].key < entries[j].key })
		for _, entry := range entries {
			if err := in.collect(entry.value, uintptr(index), entry.key, true, depth+1); err != nil {
				return err
			}
		}
	case reflect.Struct:
		if depth >= 32 {
			return &Error{Kind: ErrorUsage, Message: "container nesting exceeds limit"}
		}
		plan, err := inputPlanFor(value.Type())
		if err != nil {
			return err
		}
		index := in.add(parent, key, hasKey, depth, 7)
		for _, field := range plan.fields {
			next, present := inputFieldValue(value, field.index)
			if !present {
				continue
			}
			if field.omitEmpty && inputEmpty(next) || field.omitZero != nil && field.omitZero(next) {
				continue
			}
			if field.quoted != nil {
				wrapper := reflect.New(field.quoted).Elem()
				wrapper.Field(0).Set(next)
				var source any = wrapper.Interface()
				if next.CanAddr() {
					source = wrapper.Addr().Interface()
				}
				encoded, err := json.Marshal(source)
				if err != nil {
					return err
				}
				// The sole exported field always has the constant JSON key x.
				child := in.add(uintptr(index), field.name, true, depth+1, inputJSON)
				in.nodes[child].text = in.bytes(encoded[5 : len(encoded)-1])
				continue
			}
			if err := in.collect(next, uintptr(index), field.name, true, depth+1); err != nil {
				return err
			}
		}
	default:
		return &json.UnsupportedTypeError{Type: value.Type()}
	}
	return nil
}
func typeHasMarshalMethod(t reflect.Type) bool {
	return t.Implements(marshalType) || reflect.PointerTo(t).Implements(marshalType) || t.Implements(textMarshalType) || reflect.PointerTo(t).Implements(textMarshalType)
}
func inputMapKey(value reflect.Value) (string, error) {
	if value.Kind() == reflect.String {
		return value.String(), nil
	}
	if value.Type().Implements(textMarshalType) {
		if value.Kind() == reflect.Pointer && value.IsNil() {
			return "", nil
		}
		bytes, err := value.Interface().(encoding.TextMarshaler).MarshalText()
		// A later key callback may reuse the returned buffer.
		return string(bytes), err
	}
	switch value.Kind() {
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64:
		return strconv.FormatInt(value.Int(), 10), nil
	case reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64, reflect.Uintptr:
		return strconv.FormatUint(value.Uint(), 10), nil
	}
	return "", &json.UnsupportedTypeError{Type: value.Type()}
}
func inputEmpty(value reflect.Value) bool {
	switch value.Kind() {
	case reflect.Array, reflect.Map, reflect.Slice, reflect.String:
		return value.Len() == 0
	case reflect.Bool, reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64, reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64, reflect.Uintptr, reflect.Float32, reflect.Float64, reflect.Interface, reflect.Pointer:
		return value.IsZero()
	}
	return false
}

type inputField struct {
	index     []int
	name      string
	omitEmpty bool
	quoted    reflect.Type
	omitZero  func(reflect.Value) bool
	tagged    bool
}
type inputPlan struct{ fields []inputField }
type inputPlanResult struct {
	plan *inputPlan
	err  error
}

var inputPlans sync.Map

func inputPlanFor(t reflect.Type) (*inputPlan, error) {
	if cached, ok := inputPlans.Load(t); ok {
		result := cached.(inputPlanResult)
		return result.plan, result.err
	}
	type scan struct {
		typ   reflect.Type
		index []int
	}
	next := []scan{{typ: t}}
	visited := map[reflect.Type]bool{}
	var counts map[reflect.Type]int
	var fields []inputField
	var planErr error
	for len(next) > 0 && planErr == nil {
		current := next
		next = nil
		nextCounts := map[reflect.Type]int{}
		for _, parent := range current {
			if visited[parent.typ] {
				continue
			}
			visited[parent.typ] = true
			for i := 0; i < parent.typ.NumField(); i++ {
				field := parent.typ.Field(i)
				if field.Anonymous {
					underlying := field.Type
					if underlying.Kind() == reflect.Pointer {
						underlying = underlying.Elem()
					}
					if !field.IsExported() && underlying.Kind() != reflect.Struct {
						continue
					}
				} else if !field.IsExported() {
					continue
				}
				tag := field.Tag.Get("json")
				if tag == "-" {
					continue
				}
				options := strings.Split(tag, ",")
				name := options[0]
				for _, r := range name {
					if !unicode.IsLetter(r) && !unicode.IsDigit(r) && !strings.ContainsRune("!#$%&()*+-./:;<=>?@[]^_{|}~ ", r) {
						name = ""
						break
					}
				}
				index := append(append([]int(nil), parent.index...), i)
				underlying := field.Type
				if underlying.Name() == "" && underlying.Kind() == reflect.Pointer {
					underlying = underlying.Elem()
				}
				if name == "" && field.Anonymous && underlying.Kind() == reflect.Struct {
					nextCounts[underlying]++
					if nextCounts[underlying] == 1 {
						next = append(next, scan{typ: underlying, index: index})
					}
					continue
				}
				tagged := name != ""
				if name == "" {
					name = field.Name
				}
				entry := inputField{index: index, name: name, tagged: tagged}
				for _, option := range options[1:] {
					switch option {
					case "omitempty":
						entry.omitEmpty = true
					case "omitzero":
						entry.omitZero = inputOmitZero(field.Type)
					case "string":
						switch underlying.Kind() {
						case reflect.Bool, reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64, reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64, reflect.Uintptr, reflect.Float32, reflect.Float64, reflect.String:
							entry.quoted = reflect.StructOf([]reflect.StructField{{Name: "X", Type: field.Type, Tag: reflect.StructTag(`json:"x,string"`)}})
						}
					}
				}
				fields = append(fields, entry)
				// Multiple paths at this depth must participate in ambiguity.
				if counts[parent.typ] > 1 {
					fields = append(fields, entry)
				}
			}
		}
		counts = nextCounts
	}
	lessIndex := func(a, b []int) bool {
		for i := 0; i < len(a) && i < len(b); i++ {
			if a[i] != b[i] {
				return a[i] < b[i]
			}
		}
		return len(a) < len(b)
	}
	sort.Slice(fields, func(i, j int) bool {
		a, b := fields[i], fields[j]
		if a.name != b.name {
			return a.name < b.name
		}
		if len(a.index) != len(b.index) {
			return len(a.index) < len(b.index)
		}
		if a.tagged != b.tagged {
			return a.tagged
		}
		return lessIndex(a.index, b.index)
	})
	selected := fields[:0]
	for i := 0; i < len(fields); {
		end := i + 1
		for end < len(fields) && fields[end].name == fields[i].name {
			end++
		}
		if end == i+1 || len(fields[i].index) != len(fields[i+1].index) || fields[i].tagged != fields[i+1].tagged {
			selected = append(selected, fields[i])
		}
		i = end
	}
	sort.Slice(selected, func(i, j int) bool { return lessIndex(selected[i].index, selected[j].index) })
	plan := &inputPlan{fields: selected}
	actual, _ := inputPlans.LoadOrStore(t, inputPlanResult{plan: plan, err: planErr})
	result := actual.(inputPlanResult)
	return result.plan, result.err
}
func inputFieldValue(value reflect.Value, index []int) (reflect.Value, bool) {
	for _, field := range index {
		if value.Kind() == reflect.Pointer {
			if value.IsNil() {
				return reflect.Value{}, false
			}
			value = value.Elem()
		}
		value = value.Field(field)
	}
	return value, true
}
