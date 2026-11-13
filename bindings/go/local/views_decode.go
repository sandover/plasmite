package local

import (
	"encoding"
	"encoding/json"
	"reflect"
	"strings"
	"sync"
	"unicode"
)

type decodeField struct {
	index []int
	slot  int
	kind  reflect.Kind
	bits  int
}
type decodeStructObject struct{ slot, count int }
type decodePlan struct {
	selector *Selector
	fields   []decodeField
	objects  []decodeStructObject
}
type decodePlanResult struct{ plan *decodePlan }

var decodePlans sync.Map
var jsonUnmarshaler = reflect.TypeOf((*json.Unmarshaler)(nil)).Elem()
var textUnmarshaler = reflect.TypeOf((*encoding.TextUnmarshaler)(nil)).Elem()
var jsonNumberType = reflect.TypeOf(json.Number(""))

func hasDecodeHook(t reflect.Type) bool {
	return t.Implements(jsonUnmarshaler) || reflect.PointerTo(t).Implements(jsonUnmarshaler) || t.Implements(textUnmarshaler) || reflect.PointerTo(t).Implements(textUnmarshaler) || t == jsonNumberType
}
func planFor(t reflect.Type) *decodePlan {
	if result, ok := decodePlans.Load(t); ok {
		return result.(decodePlanResult).plan
	}
	plan := &decodePlan{}
	var paths []string
	var walk func(reflect.Type, []int, string) bool
	walk = func(t reflect.Type, index []int, path string) bool {
		if t.Kind() != reflect.Struct || hasDecodeHook(t) {
			return false
		}
		object := decodeStructObject{slot: len(paths)}
		paths = append(paths, path)
		names := map[string]bool{}
		for i := 0; i < t.NumField(); i++ {
			f := t.Field(i)
			if f.Anonymous {
				return false
			}
			if !f.IsExported() {
				continue
			}
			options := strings.Split(f.Tag.Get("json"), ",")
			if options[0] == "-" {
				continue
			}
			for _, option := range options[1:] {
				if option == "string" {
					return false
				}
			}
			name := options[0]
			if name == "" {
				name = f.Name
			}
			// Delegate unusual tag syntax to encoding/json's own field rules.
			for _, r := range name {
				if !unicode.IsLetter(r) && !unicode.IsDigit(r) && !strings.ContainsRune("!#$%&()*+-./:;<=>?@[]^_{|}~ ", r) {
					return false
				}
			}
			if names[name] {
				return false
			}
			names[name] = true
			object.count++
			childIndex := append(append([]int(nil), index...), i)
			childPath := path + "/" + strings.ReplaceAll(strings.ReplaceAll(name, "~", "~0"), "/", "~1")
			if hasDecodeHook(f.Type) {
				return false
			}
			if f.Type.Kind() == reflect.Struct {
				if !walk(f.Type, childIndex, childPath) {
					return false
				}
				continue
			}
			switch f.Type.Kind() {
			case reflect.Bool, reflect.String, reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64, reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64, reflect.Uintptr, reflect.Float64:
				bits := 0
				if f.Type.Kind() != reflect.Bool && f.Type.Kind() != reflect.String {
					bits = f.Type.Bits()
				}
				plan.fields = append(plan.fields, decodeField{index: childIndex, slot: len(paths), kind: f.Type.Kind(), bits: bits})
				paths = append(paths, childPath)
			default:
				return false
			}
		}
		plan.objects = append(plan.objects, object)
		return true
	}
	if !walk(t, nil, "") {
		plan = nil
	} else {
		selector, err := Prepare(paths...)
		if err != nil {
			plan = nil
		} else {
			plan.selector = selector
		}
	}
	actual, loaded := decodePlans.LoadOrStore(t, decodePlanResult{plan})
	if loaded && plan != nil {
		plan.selector.Close()
	}
	return actual.(decodePlanResult).plan
}

// Fast decoding accepts complete ordinary struct shapes with exact field names.
// This condition also excludes extra case variants which encoding/json could
// match later. Missing/null fields, custom hooks, embedded fields, maps, slices,
// pointers and conversion errors retain encoding/json's full semantics.
func (v Value) decodeStruct(destination any) (bool, error) {
	target := reflect.ValueOf(destination)
	if target.Kind() != reflect.Pointer || target.IsNil() || target.Elem().Kind() != reflect.Struct {
		return false, nil
	}
	plan := planFor(target.Elem().Type())
	if plan == nil {
		return false, nil
	}
	slots, err := plan.selector.read(v.owner, v.node)
	if err != nil {
		return true, err
	}
	for _, object := range plan.objects {
		slot := slots[object.slot]
		if slot.kind != 7 || uint64(slot.len) != uint64(object.count) {
			return false, nil
		}
	}
	for _, field := range plan.fields {
		slot := slots[field.slot]
		switch field.kind {
		case reflect.String:
			if slot.kind != 5 {
				return false, nil
			}
		case reflect.Bool:
			if slot.kind != 2 {
				return false, nil
			}
		case reflect.Float64:
			if slot.kind != 3 && slot.kind != 4 {
				return false, nil
			}
		case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64:
			if slot.kind != 3 {
				return false, nil
			}
			n := int64(slot.integer)
			if field.bits < 64 && (n < -(int64(1)<<(field.bits-1)) || n > (int64(1)<<(field.bits-1))-1) {
				return false, nil
			}
		default:
			if slot.kind != 3 || slot.integer < 0 {
				return false, nil
			}
			if field.bits < 64 && uint64(slot.integer) > (uint64(1)<<field.bits)-1 {
				return false, nil
			}
		}
	}
	root := target.Elem()
	for _, field := range plan.fields {
		slot := slots[field.slot]
		target := root.FieldByIndex(field.index)
		switch field.kind {
		case reflect.String:
			target.SetString(string(v.owner.bytes()[int(slot.offset):int(slot.offset+slot.len)]))
		case reflect.Bool:
			target.SetBool(slot.integer != 0)
		case reflect.Float64:
			if slot.kind == 3 {
				target.SetFloat(float64(slot.integer))
			} else {
				target.SetFloat(float64(slot.real))
			}
		case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64:
			target.SetInt(int64(slot.integer))
		default:
			target.SetUint(uint64(slot.integer))
		}
	}
	return true, nil
}
