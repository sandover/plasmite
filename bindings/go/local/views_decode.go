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
type decodeStructObject struct{ slot int }
type decodePlan struct {
	decoder *structDecoder
	fields  []decodeField
	objects []decodeStructObject
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
			if f.Tag.Get("json") == "-" {
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
			if !isASCII(name) {
				return false
			}
			lower := strings.ToLower(name)
			if names[lower] {
				return false
			}
			names[lower] = true

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
			case reflect.Slice:
				if f.Type.Elem().Kind() != reflect.Uint8 {
					return false
				}
				plan.fields = append(plan.fields, decodeField{index: childIndex, slot: len(paths), kind: reflect.Slice})
				paths = append(paths, childPath)

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
		selector, err := prepareSelector(paths...)
		if err != nil {
			plan = nil
		} else {
			objects := make([]byte, len(paths))
			for _, object := range plan.objects {
				objects[object.slot] = 1
			}
			decoder, err := prepareStructDecoder(selector, objects)
			selector.Close()
			if err != nil {
				plan = nil
			} else {
				plan.decoder = decoder
			}
		}
	}
	actual, loaded := decodePlans.LoadOrStore(t, decodePlanResult{plan})
	if loaded && plan != nil {
		plan.decoder.close()
	}
	return actual.(decodePlanResult).plan
}

func isASCII(text string) bool {
	for i := 0; i < len(text); i++ {
		if text[i] >= 128 {
			return false
		}
	}
	return true
}

// Native dispatch scans each selected object once and preserves canonical JSON
// field precedence. Unicode or ambiguous folded names use the standard decoder.
// Validate every assignment first so errors keep JSON's partial-update behavior.
func (v viewValue) decodeStruct(destination any) (bool, error) {
	target := reflect.ValueOf(destination)
	if target.Kind() != reflect.Pointer || target.IsNil() || target.Elem().Kind() != reflect.Struct {
		return false, nil
	}
	plan := planFor(target.Elem().Type())
	if plan == nil {
		return false, nil
	}
	slots, supported, err := plan.decoder.read(v.owner, v.node)
	if err != nil {
		return true, err
	}
	if !supported {
		return false, nil
	}
	for _, object := range plan.objects {
		slot := slots[object.slot]
		if slot.kind != 0 && slot.kind != 7 {
			return false, nil
		}
	}
	for _, field := range plan.fields {
		slot := slots[field.slot]
		if slot.kind == 0 {
			continue
		}
		switch field.kind {
		case reflect.Slice:
			if slot.kind != 6 {
				return false, nil
			}
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
		if slot.kind == 0 {
			continue
		}
		target := root.FieldByIndex(field.index)
		switch field.kind {
		case reflect.Slice:
			output := make([]byte, int(slot.len))
			copy(output, v.owner.bytes()[int(slot.offset):int(slot.offset+slot.len)])
			target.SetBytes(output)
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
