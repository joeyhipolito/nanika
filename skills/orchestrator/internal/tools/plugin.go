package tools

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"log"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"
)

// pluginJSON is the minimal shape of plugin.json we care about.
type pluginJSON struct {
	Name   string `json:"name"`
	Binary string `json:"binary"`

	Capabilities struct {
		Commands map[string]pluginCommand `json:"commands"`
	} `json:"capabilities"`
}

type pluginCommand struct {
	Description string   `json:"description"`
	Args        []string `json:"args"`
}

// helpJSON is the optional response from <binary> --help-json.
type helpJSON struct {
	Name     string         `json:"name"`
	Commands []helpJSONVerb `json:"commands"`
}

type helpJSONVerb struct {
	Name        string        `json:"name"`
	Description string        `json:"description"`
	Args        []helpJSONArg `json:"args"`
}

type helpJSONArg struct {
	Name        string `json:"name"`
	Type        string `json:"type"` // string | integer | boolean
	Description string `json:"description"`
	Required    bool   `json:"required"`
}

// PluginTool wraps a single plugin command as a Tool.
type PluginTool struct {
	name        string
	description string
	schema      json.RawMessage
	binary      string
	verb        string
}

func (p *PluginTool) Name() string                 { return p.name }
func (p *PluginTool) Description() string          { return p.description }
func (p *PluginTool) InputSchema() json.RawMessage { return p.schema }
func (p *PluginTool) Risk() RiskTier               { return RiskCritical } // conservative

func (p *PluginTool) Execute(ctx context.Context, args map[string]any) (ToolResult, error) {
	cmdArgs := []string{p.verb}

	for k, v := range args {
		switch val := v.(type) {
		case bool:
			if val {
				cmdArgs = append(cmdArgs, "--"+k)
			}
		case string:
			if val != "" {
				cmdArgs = append(cmdArgs, "--"+k, val)
			}
		default:
			if f, ok := toFloat(v); ok {
				cmdArgs = append(cmdArgs, "--"+k, fmt.Sprintf("%g", f))
			}
		}
	}

	var buf bytes.Buffer
	cmd := exec.CommandContext(ctx, p.binary, cmdArgs...)
	cmd.Stdout = &buf
	cmd.Stderr = &buf
	if err := cmd.Run(); err != nil {
		content := buf.String()
		if content == "" {
			content = err.Error()
		}
		return ToolResult{IsError: true, Content: content}, nil
	}
	return ToolResult{Content: buf.String()}, nil
}

// pluginTools discovers plugins under pluginsDir and generates Tool instances.
// For each plugin:
//  1. Reads plugin.json for the binary name and command list.
//  2. Attempts <binary> --help-json for richer schemas (falls back to plugin.json).
//  3. Produces one PluginTool per discovered command.
func pluginTools(ctx context.Context, pluginsDir string) []Tool {
	if pluginsDir == "" {
		return nil
	}
	entries, err := os.ReadDir(pluginsDir)
	if err != nil {
		return nil
	}

	var out []Tool
	for _, entry := range entries {
		if !entry.IsDir() {
			continue
		}
		pjPath := filepath.Join(pluginsDir, entry.Name(), "plugin.json")
		data, err := os.ReadFile(pjPath)
		if err != nil {
			continue
		}
		var pj pluginJSON
		if err := json.Unmarshal(data, &pj); err != nil {
			continue
		}
		if pj.Binary == "" {
			pj.Binary = pj.Name
		}
		binary := resolveBinary(pj.Binary)

		tools := toolsFromHelpJSON(ctx, binary, pj.Name)
		if len(tools) == 0 {
			tools = toolsFromPluginJSON(binary, pj.Name, pj.Capabilities.Commands)
		}
		for _, t := range tools {
			out = append(out, t)
		}
	}
	return out
}

// toolsFromHelpJSON calls <binary> --help-json and parses the response.
func toolsFromHelpJSON(ctx context.Context, binary, pluginName string) []*PluginTool {
	if binary == "" {
		return nil
	}
	ctx2, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()

	var buf bytes.Buffer
	cmd := exec.CommandContext(ctx2, binary, "--help-json")
	cmd.Stdout = &buf
	if err := cmd.Run(); err != nil {
		return nil
	}

	var h helpJSON
	if err := json.Unmarshal(buf.Bytes(), &h); err != nil {
		return nil
	}

	var tools []*PluginTool
	for _, verb := range h.Commands {
		toolName := sanitizeName(pluginName + "_" + verb.Name)
		props := make(map[string]Prop, len(verb.Args))
		var required []string
		for _, arg := range verb.Args {
			addSanitizedProp(props, arg.Name, Prop{
				Type:        coerceType(arg.Type),
				Description: arg.Description,
			}, toolName, &required, arg.Required)
		}
		tools = append(tools, &PluginTool{
			name:        toolName,
			description: verb.Description,
			schema:      BuildSchema(props, required),
			binary:      binary,
			verb:        verb.Name,
		})
	}
	return tools
}

// toolsFromPluginJSON generates PluginTools from capabilities.commands.
func toolsFromPluginJSON(binary, pluginName string, commands map[string]pluginCommand) []*PluginTool {
	var tools []*PluginTool
	for verb, cmd := range commands {
		props := parseArgStrings(cmd.Args)
		tools = append(tools, &PluginTool{
			name:        sanitizeName(pluginName + "_" + verb),
			description: cmd.Description,
			schema:      BuildSchema(props, nil),
			binary:      binary,
			verb:        verb,
		})
	}
	return tools
}

// parseArgStrings converts plugin.json args strings to schema props.
// "--name <value>" → string prop; "--flag" → boolean prop; "<pos>" → string prop.
// All resulting keys are passed through sanitizePropKey so they conform to
// Anthropic's property-name regex ^[a-zA-Z0-9_.-]{1,64}$.
func parseArgStrings(args []string) map[string]Prop {
	props := make(map[string]Prop)
	for _, a := range args {
		a = strings.TrimSpace(a)
		if !strings.HasPrefix(a, "--") {
			name := strings.Trim(a, "<>[]")
			name = sanitizePropKey(name)
			if name != "" && name != "." {
				props[name] = String("positional argument")
			}
			continue
		}
		parts := strings.Fields(a)
		key := strings.TrimPrefix(parts[0], "--")
		if idx := strings.IndexAny(key, "|<"); idx > 0 {
			key = key[:idx]
		}
		key = sanitizePropKey(key)
		if key == "" {
			continue
		}
		if len(parts) == 1 {
			props[key] = Bool(a)
		} else {
			props[key] = String(a)
		}
	}
	return props
}

// resolveBinary finds the installed binary, checking ~/.alluka/bin first.
func resolveBinary(name string) string {
	home, err := os.UserHomeDir()
	if err == nil {
		installed := filepath.Join(home, ".alluka", "bin", name)
		if _, err := os.Stat(installed); err == nil {
			return installed
		}
	}
	if p, err := exec.LookPath(name); err == nil {
		return p
	}
	return name
}

// sanitizeName converts a "plugin_verb" string to a safe tool name (lowercase + underscores).
func sanitizeName(s string) string {
	var b strings.Builder
	for _, r := range strings.ToLower(s) {
		if (r >= 'a' && r <= 'z') || (r >= '0' && r <= '9') || r == '_' {
			b.WriteRune(r)
		} else if r == '-' || r == ' ' {
			b.WriteByte('_')
		}
	}
	return b.String()
}

// sanitizePropKey rewrites a JSON-schema property key so it conforms to
// Anthropic's tool input_schema regex: ^[a-zA-Z0-9_.-]{1,64}$.
// Any disallowed rune is replaced with '_', and the result is truncated
// to 64 bytes. Empty input returns "" (callers should drop empties).
func sanitizePropKey(s string) string {
	if s == "" {
		return ""
	}
	var b strings.Builder
	b.Grow(len(s))
	for _, r := range s {
		switch {
		case r >= 'a' && r <= 'z',
			r >= 'A' && r <= 'Z',
			r >= '0' && r <= '9',
			r == '_', r == '.', r == '-':
			b.WriteRune(r)
		default:
			b.WriteByte('_')
		}
	}
	out := b.String()
	if len(out) > 64 {
		out = out[:64]
	}
	return out
}

// addSanitizedProp inserts (key, prop) into props using a sanitized key.
// If sanitization changes the key, it logs the rewrite. On collision after
// sanitization, the new entry is dropped with a warning so we never silently
// overwrite a previously-registered property. If addToRequired is non-nil and
// non-empty, the sanitized key is appended to it.
func addSanitizedProp(props map[string]Prop, raw string, p Prop, toolName string, required *[]string, isRequired bool) {
	clean := sanitizePropKey(raw)
	if clean == "" {
		log.Printf("tools/plugin: dropping empty property key for tool %q (raw=%q)", toolName, raw)
		return
	}
	if clean != raw {
		log.Printf("tools/plugin: sanitized property key %q -> %q for tool %q", raw, clean, toolName)
	}
	if _, exists := props[clean]; exists && clean != raw {
		log.Printf("tools/plugin: dropping property %q on tool %q: collides with existing %q after sanitization", raw, toolName, clean)
		return
	}
	props[clean] = p
	if isRequired && required != nil {
		*required = append(*required, clean)
	}
}

// coerceType maps help-json type strings to JSON Schema types.
func coerceType(t string) string {
	switch strings.ToLower(t) {
	case "integer", "int", "number":
		return "integer"
	case "boolean", "bool":
		return "boolean"
	default:
		return "string"
	}
}
