// Hand-written compatibility helpers beside the generated Protobuf bindings.
//
// The generated types accept proto3 JSON through protojson. Two policies from
// proto/v1/WIRE.md are not protojson defaults, so this file owns them:
//
//  1. Unknown field names are additive and must be ignored.
//  2. An unknown enum value must be an explicit error, never the zero value.
//
// A oneof payload still has to be checked per message with RequireOneof.

package whalereinv1

import (
	"encoding/json"
	"fmt"

	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/reflect/protoreflect"
)

// UnknownEnumError reports an enum value that this build does not know.
type UnknownEnumError struct {
	Field string
	Name  string
	Value int32
}

func (e *UnknownEnumError) Error() string {
	if e.Name != "" {
		return fmt.Sprintf("unknown enum name %q for %s", e.Name, e.Field)
	}
	return fmt.Sprintf("unknown enum value %d for %s", e.Value, e.Field)
}

// MissingOneofError reports a oneof that carries no known variant.
type MissingOneofError struct {
	Message string
	Oneof   string
}

func (e *MissingOneofError) Error() string {
	return fmt.Sprintf("%s carries no known %s variant", e.Message, e.Oneof)
}

// DecodeJSON decodes proto3 JSON with the additive-field policy and rejects any
// enum value the current schema does not know.
func DecodeJSON(data []byte, message proto.Message) error {
	// protojson drops an unknown enum *name* together with unknown fields when
	// DiscardUnknown is set, so the raw document is checked against the
	// descriptor before the lenient decode can erase the evidence.
	if err := checkJSONEnums(data, message.ProtoReflect().Descriptor()); err != nil {
		return err
	}
	options := protojson.UnmarshalOptions{DiscardUnknown: true}
	if err := options.Unmarshal(data, message); err != nil {
		return err
	}
	return CheckKnownEnums(message)
}

func checkJSONEnums(data []byte, descriptor protoreflect.MessageDescriptor) error {
	var object map[string]any
	if err := json.Unmarshal(data, &object); err != nil {
		return err
	}
	return walkJSON(descriptor, object)
}

func walkJSON(descriptor protoreflect.MessageDescriptor, object map[string]any) error {
	fields := descriptor.Fields()
	for key, value := range object {
		field := fields.ByJSONName(key)
		if field == nil {
			field = fields.ByName(protoreflect.Name(key))
		}
		if field == nil {
			continue // additive field
		}
		if err := walkJSONValue(field, value); err != nil {
			return err
		}
	}
	return nil
}

func walkJSONValue(field protoreflect.FieldDescriptor, value any) error {
	if value == nil {
		return nil
	}
	if field.IsMap() {
		entries, ok := value.(map[string]any)
		if !ok {
			return nil
		}
		for _, entry := range entries {
			if err := walkJSONFieldValue(field.MapValue(), entry); err != nil {
				return err
			}
		}
		return nil
	}
	if field.IsList() {
		entries, ok := value.([]any)
		if !ok {
			return nil
		}
		for _, entry := range entries {
			if err := walkJSONFieldValue(field, entry); err != nil {
				return err
			}
		}
		return nil
	}
	return walkJSONFieldValue(field, value)
}

func walkJSONFieldValue(field protoreflect.FieldDescriptor, value any) error {
	switch field.Kind() {
	case protoreflect.EnumKind:
		name, ok := value.(string)
		if !ok {
			return nil // numeric enums are checked after decode
		}
		if field.Enum().Values().ByName(protoreflect.Name(name)) == nil {
			return &UnknownEnumError{Field: string(field.FullName()), Name: name}
		}
		return nil
	case protoreflect.MessageKind, protoreflect.GroupKind:
		object, ok := value.(map[string]any)
		if !ok {
			return nil
		}
		return walkJSON(field.Message(), object)
	default:
		return nil
	}
}

// CheckKnownEnums walks every populated enum field (including repeated and map
// entries) and fails when a value has no name in the schema.
func CheckKnownEnums(message proto.Message) error {
	return walkEnums(message.ProtoReflect())
}

func walkEnums(value protoreflect.Message) error {
	var failure error
	value.Range(func(field protoreflect.FieldDescriptor, item protoreflect.Value) bool {
		if field.IsMap() {
			// Map values are either messages or enums; keys are scalar.
			if field.MapValue().Kind() == protoreflect.EnumKind {
				item.Map().Range(func(_ protoreflect.MapKey, mapValue protoreflect.Value) bool {
					if err := checkEnum(field, mapValue.Enum()); err != nil {
						failure = err
						return false
					}
					return true
				})
			}
			return failure == nil
		}
		switch field.Kind() {
		case protoreflect.EnumKind:
			if field.IsList() {
				list := item.List()
				for index := 0; index < list.Len(); index++ {
					if err := checkEnum(field, list.Get(index).Enum()); err != nil {
						failure = err
						return false
					}
				}
				return true
			}
			failure = checkEnum(field, item.Enum())
			return failure == nil
		case protoreflect.MessageKind, protoreflect.GroupKind:
			if field.IsList() {
				list := item.List()
				for index := 0; index < list.Len(); index++ {
					if err := walkEnums(list.Get(index).Message()); err != nil {
						failure = err
						return false
					}
				}
				return true
			}
			failure = walkEnums(item.Message())
			return failure == nil
		default:
			return true
		}
	})
	return failure
}

func checkEnum(field protoreflect.FieldDescriptor, number protoreflect.EnumNumber) error {
	if field.Enum().Values().ByNumber(number) == nil {
		return &UnknownEnumError{Field: string(field.FullName()), Value: int32(number)}
	}
	return nil
}

// RequireOneof fails when a oneof carries no known variant, so a newer peer's
// unknown variant can never look like an empty success.
func RequireOneof(message proto.Message, oneof string) error {
	descriptor := message.ProtoReflect().Descriptor()
	field := descriptor.Oneofs().ByName(protoreflect.Name(oneof))
	if field == nil {
		return fmt.Errorf("%s has no oneof %s", descriptor.FullName(), oneof)
	}
	if message.ProtoReflect().WhichOneof(field) == nil {
		return &MissingOneofError{Message: string(descriptor.FullName()), Oneof: oneof}
	}
	return nil
}
