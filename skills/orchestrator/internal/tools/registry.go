package tools

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"sync"
)

// Registry holds all registered tools indexed by name.
type Registry struct {
	mu    sync.RWMutex
	tools map[string]Tool
}

// New returns an empty Registry.
func New() *Registry {
	return &Registry{tools: make(map[string]Tool)}
}

// Register adds a tool. Panics on duplicate names (consistent with http.ServeMux).
func (r *Registry) Register(t Tool) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if _, exists := r.tools[t.Name()]; exists {
		panic(fmt.Sprintf("tools: duplicate registration: %q", t.Name()))
	}
	r.tools[t.Name()] = t
}

// Get returns the named tool, or nil if not found.
func (r *Registry) Get(name string) Tool {
	r.mu.RLock()
	defer r.mu.RUnlock()
	return r.tools[name]
}

// All returns all registered tools sorted by name.
func (r *Registry) All() []Tool {
	r.mu.RLock()
	defer r.mu.RUnlock()
	out := make([]Tool, 0, len(r.tools))
	for _, t := range r.tools {
		out = append(out, t)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Name() < out[j].Name() })
	return out
}

// Len returns the count of registered tools.
func (r *Registry) Len() int {
	r.mu.RLock()
	defer r.mu.RUnlock()
	return len(r.tools)
}

// LoadOption configures Load.
type LoadOption func(*loadConfig)

type loadConfig struct {
	pluginsDir string
	nanikaDir  string
	todosDir   string
	sessionID  string
}

// WithPluginsDir sets a custom plugins directory for tier-2 discovery.
func WithPluginsDir(dir string) LoadOption {
	return func(c *loadConfig) { c.pluginsDir = dir }
}

// WithNanikaDir sets the nanika home directory used for skill index loading.
func WithNanikaDir(dir string) LoadOption {
	return func(c *loadConfig) { c.nanikaDir = dir }
}

// WithTodosDir sets the directory for todo files (default: ~/.alluka/todos).
func WithTodosDir(dir string) LoadOption {
	return func(c *loadConfig) { c.todosDir = dir }
}

// WithSessionID scopes the todo tool to a specific session.
func WithSessionID(id string) LoadOption {
	return func(c *loadConfig) { c.sessionID = id }
}

// Load builds and returns a Registry with all tiers registered.
// ctx is passed to plugin discovery (short-lived --help-json calls).
func Load(ctx context.Context, opts ...LoadOption) *Registry {
	cfg := loadConfig{
		pluginsDir: defaultPluginsDir(),
		nanikaDir:  defaultNanikaDir(),
		todosDir:   defaultTodosDir(),
	}
	for _, o := range opts {
		o(&cfg)
	}

	r := New()

	// Tier 1 — built-in primitives
	for _, t := range tier1Tools() {
		r.Register(t)
	}

	// Tier 2 — plugin-generated (skip on name collision with tier 1)
	for _, t := range pluginTools(ctx, cfg.pluginsDir) {
		if r.Get(t.Name()) == nil {
			r.Register(t)
		}
	}

	// Tier 3 — nanika-native
	for _, t := range tier3Tools(cfg) {
		if r.Get(t.Name()) == nil {
			r.Register(t)
		}
	}

	return r
}

// tier1Tools returns the six built-in primitives.
func tier1Tools() []Tool {
	return []Tool{
		NewBashTool(),
		NewFileReadTool(),
		NewFileWriteTool(),
		NewFileEditTool(),
		NewGlobTool(),
		NewGrepTool(),
	}
}

// tier3Tools returns the nanika-native tools.
func tier3Tools(cfg loadConfig) []Tool {
	return []Tool{
		NewSkillViewTool(cfg.nanikaDir),
		NewSkillSearchTool(cfg.nanikaDir),
		NewSessionSearchTool(),
		NewTodoTool(cfg.todosDir, cfg.sessionID),
	}
}

func defaultPluginsDir() string {
	if dir := os.Getenv("ORCHESTRATOR_NANIKA_DIR"); dir != "" {
		return filepath.Join(dir, "plugins")
	}
	if dir := os.Getenv("ORCHESTRATOR_VIA_DIR"); dir != "" {
		return filepath.Join(dir, "plugins")
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}
	return filepath.Join(home, "nanika", "plugins")
}

func defaultNanikaDir() string {
	if dir := os.Getenv("ORCHESTRATOR_NANIKA_DIR"); dir != "" {
		return dir
	}
	if dir := os.Getenv("ORCHESTRATOR_VIA_DIR"); dir != "" {
		return dir
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "."
	}
	return filepath.Join(home, "nanika")
}

func defaultTodosDir() string {
	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}
	return filepath.Join(home, ".alluka", "todos")
}
