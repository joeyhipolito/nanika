package tools

import "encoding/json"

const draft7URI = "http://json-schema.org/draft-07/schema#"

// Prop describes a single JSON Schema property.
type Prop struct {
	Type        string   `json:"type"`
	Description string   `json:"description,omitempty"`
	Enum        []string `json:"enum,omitempty"`
	Default     any      `json:"default,omitempty"`
	Items       *Prop    `json:"items,omitempty"` // for array types
	Minimum     *int     `json:"minimum,omitempty"`
}

type schemaDoc struct {
	Schema     string          `json:"$schema"`
	Type       string          `json:"type"`
	Properties map[string]Prop `json:"properties,omitempty"`
	Required   []string        `json:"required,omitempty"`
}

// BuildSchema returns a JSON Schema draft-7 object for use in InputSchema().
func BuildSchema(props map[string]Prop, required []string) json.RawMessage {
	doc := schemaDoc{
		Schema:     draft7URI,
		Type:       "object",
		Properties: props,
		Required:   required,
	}
	b, _ := json.Marshal(doc)
	return json.RawMessage(b)
}

// String returns a string Prop.
func String(description string) Prop {
	return Prop{Type: "string", Description: description}
}

// Integer returns an integer Prop with optional minimum.
func Integer(description string, minimum *int) Prop {
	return Prop{Type: "integer", Description: description, Minimum: minimum}
}

// Bool returns a boolean Prop.
func Bool(description string) Prop {
	return Prop{Type: "boolean", Description: description}
}

// Enum returns a string Prop restricted to specific values.
func Enum(description string, values ...string) Prop {
	return Prop{Type: "string", Description: description, Enum: values}
}

// ptr is a convenience to take the address of an int literal.
func ptr[T any](v T) *T { return &v }
