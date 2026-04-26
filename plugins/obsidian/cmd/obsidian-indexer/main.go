package main

import (
	"bufio"
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"os/signal"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/fsnotify/fsnotify"
	"github.com/joeyhipolito/nanika-obsidian/internal/graph"
	"github.com/joeyhipolito/nanika-obsidian/internal/index"
	"github.com/joeyhipolito/nanika-obsidian/internal/recall"
	"github.com/joeyhipolito/nanika-obsidian/internal/rpc"
	vaultpkg "github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

var (
	pidFile  string
	statsFile string
)

func init() {
	home, _ := os.UserHomeDir()
	runtimeDir := filepath.Join(home, ".cache", "nanika", "obsidian")
	pidFile = filepath.Join(runtimeDir, "indexer.pid")
	statsFile = filepath.Join(runtimeDir, "stats.json")
}

type daemonStats struct {
	StartedAt        time.Time `json:"started_at"`
	PID              int       `json:"pid"`
	LastRegenAt      time.Time `json:"last_regen_at"`
	RegenCount       int64     `json:"regen_count"`
	Uptime           string    `json:"uptime"`
	IndexNotes       int       `json:"index_notes"`
	IndexLinks       int       `json:"index_links"`
	IndexLastUpdate  time.Time `json:"index_last_update"`
	GraphVertices    int       `json:"graph_vertices"`
	GraphEdges       int       `json:"graph_edges"`
	GraphLastRebuild time.Time `json:"graph_last_rebuild"`

	RPCSocketPath    string `json:"rpc_socket_path"`
	RPCActiveConns   int64  `json:"rpc_active_conns"`
	RPCRequestsTotal int64  `json:"rpc_requests_total"`
	RPCErrorsTotal   int64  `json:"rpc_errors_total"`
}

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintf(os.Stderr, "usage: obsidian-indexer <serve|stop|status>\n")
		os.Exit(1)
	}

	switch os.Args[1] {
	case "serve":
		fs := flag.NewFlagSet("serve", flag.ExitOnError)
		kindFlag := fs.String("vault-kind", "", "vault schema (nanika|second-brain); overrides OBSIDIAN_VAULT_KIND")
		_ = fs.Parse(os.Args[2:])
		serveDaemon(*kindFlag)
	case "stop":
		stopDaemon()
	case "status":
		statusDaemon()
	default:
		fmt.Fprintf(os.Stderr, "unknown command: %s\n", os.Args[1])
		os.Exit(1)
	}
}

func serveDaemon(vaultKindStr string) {
	vault := resolveVault()
	vaultKind := resolveVaultKind(vaultKindStr)
	schema := vaultpkg.SchemaFor(vaultKind)
	if vault == "" {
		fmt.Fprintf(os.Stderr, "obsidian vault not configured\n")
		os.Exit(1)
	}

	if err := os.MkdirAll(filepath.Dir(pidFile), 0700); err != nil {
		fmt.Fprintf(os.Stderr, "failed to create runtime dir: %v\n", err)
		os.Exit(1)
	}

	startedAt := time.Now()
	pid := os.Getpid()

	if err := os.WriteFile(pidFile, []byte(strconv.Itoa(pid)), 0600); err != nil {
		fmt.Fprintf(os.Stderr, "failed to write pid file: %v\n", err)
		os.Exit(1)
	}

	stats := &daemonStats{
		StartedAt: startedAt,
		PID:       pid,
	}

	var (
		statsMu sync.Mutex
		rpcSrv  *rpc.Server // set after graph is up
	)
	savingStats := func() {
		statsMu.Lock()
		stats.Uptime = time.Since(startedAt).String()
		if rpcSrv != nil {
			rs := rpcSrv.Stats()
			stats.RPCSocketPath = rs.SockPath
			stats.RPCActiveConns = rs.ActiveConns
			stats.RPCRequestsTotal = rs.RequestsTotal
			stats.RPCErrorsTotal = rs.ErrorsTotal
		}
		statsMu.Unlock()
		saveStats(stats)
	}

	// Open the SQLite index with graceful degradation.
	indexPath := filepath.Join(vault, ".cache", "obsidian.db")
	var idx *index.Indexer
	if err := os.MkdirAll(filepath.Dir(indexPath), 0700); err == nil {
		var openErr error
		idx, openErr = index.OpenIndexer(indexPath)
		if openErr != nil {
			fmt.Fprintf(os.Stderr, "warning: failed to open index: %v (continuing without index)\n", openErr)
		}
	}

	// Graph: in-memory CSR protected by an RWMutex.
	var graphMu sync.RWMutex
	var currentGraph *graph.Graph

	graphBinPath := filepath.Join(vault, ".cache", "graph.bin")

	// Rebuild the CSR from the index and persist it.
	buildAndSaveGraph := func() {
		g := buildGraphFromIndex(idx)
		if err := writeGraphAtomically(g, graphBinPath); err != nil {
			fmt.Fprintf(os.Stderr, "graph: write failed: %v\n", err)
		}
		graphMu.Lock()
		currentGraph = g
		graphMu.Unlock()
		statsMu.Lock()
		stats.GraphVertices = g.VertexCount()
		stats.GraphEdges = g.EdgeCount()
		stats.GraphLastRebuild = time.Now()
		statsMu.Unlock()
		savingStats()
		fmt.Fprintf(os.Stderr, "graph: built (%d vertices, %d edges)\n", g.VertexCount(), g.EdgeCount())
	}

	// On startup: try to load an existing graph.bin; rebuild on any error.
	if f, err := os.Open(graphBinPath); err == nil {
		g, loadErr := graph.Load(f)
		f.Close()
		if loadErr == nil {
			fmt.Fprintf(os.Stderr, "graph: loaded from %s (%d vertices, %d edges)\n", graphBinPath, g.VertexCount(), g.EdgeCount())
			graphMu.Lock()
			currentGraph = g
			graphMu.Unlock()
			statsMu.Lock()
			stats.GraphVertices = g.VertexCount()
			stats.GraphEdges = g.EdgeCount()
			statsMu.Unlock()
		} else {
			fmt.Fprintf(os.Stderr, "graph: load failed (%v), rebuilding\n", loadErr)
			buildAndSaveGraph()
		}
	} else {
		buildAndSaveGraph()
	}

	// Start RPC server. Index and graph are up at this point.
	sockPath := resolveSocketPath()
	if err := os.MkdirAll(filepath.Dir(sockPath), 0700); err != nil {
		fmt.Fprintf(os.Stderr, "failed to create socket dir: %v\n", err)
		os.Exit(1)
	}
	graphFn := func() *graph.Graph {
		graphMu.RLock()
		defer graphMu.RUnlock()
		return currentGraph
	}
	recallEngine := recall.NewEngine(graphFn, idx)
	rpcSrv = rpc.New(rpc.Config{
		Store:  nil,
		Graph:  graphFn,
		Recall: recallEngine.Recall,
	})
	if err := rpcSrv.Start(sockPath); err != nil {
		fmt.Fprintf(os.Stderr, "failed to start rpc server: %v\n", err)
		os.Exit(1)
	}
	fmt.Fprintf(os.Stderr, "rpc: listening on %s\n", sockPath)

	watcher, err := fsnotify.NewWatcher()
	if err != nil {
		fmt.Fprintf(os.Stderr, "failed to create watcher: %v\n", err)
		os.Exit(1)
	}
	defer watcher.Close()

	for _, sub := range []string{schema.Daily, schema.MOCs, schema.Sessions, schema.Ideas} {
		dir := filepath.Join(vault, sub)
		if err := os.MkdirAll(dir, 0700); err == nil {
			watcher.Add(dir)
		}
	}

	sigChan := make(chan os.Signal, 1)
	signal.Notify(sigChan, syscall.SIGTERM, os.Interrupt)

	debounceTimer := time.NewTimer(time.Duration(0))
	debounceTimer.Stop()

	// Separate 500 ms debounce for graph rebuilds triggered by link changes.
	graphDebounce := time.NewTimer(time.Duration(0))
	graphDebounce.Stop()

	regenCache := func() {
		if err := regenerateCache(vault, schema); err != nil {
			fmt.Fprintf(os.Stderr, "failed to regenerate cache: %v\n", err)
		}
		statsMu.Lock()
		stats.RegenCount++
		stats.LastRegenAt = time.Now()
		statsMu.Unlock()
		savingStats()
	}

	updateIndexStats := func() {
		if idx == nil {
			return
		}
		notes, err := idx.CountNotes()
		if err != nil {
			fmt.Fprintf(os.Stderr, "failed to count notes: %v\n", err)
			return
		}
		links, err := idx.CountLinks()
		if err != nil {
			fmt.Fprintf(os.Stderr, "failed to count links: %v\n", err)
			return
		}
		statsMu.Lock()
		stats.IndexNotes = notes
		stats.IndexLinks = links
		stats.IndexLastUpdate = time.Now()
		statsMu.Unlock()
	}

	regenCache()
	updateIndexStats()

	for {
		select {
		case event, ok := <-watcher.Events:
			if !ok {
				return
			}
			if event.Op&fsnotify.Write == fsnotify.Write || event.Op&fsnotify.Create == fsnotify.Create {
				debounceTimer.Reset(100 * time.Millisecond)
				// Process index event immediately if index is available.
				if idx != nil && isMarkdownFile(event.Name) {
					indexEvent(idx, vault, event.Name)
					// Links may have changed; schedule a graph rebuild.
					graphDebounce.Reset(500 * time.Millisecond)
				}
			} else if event.Op&fsnotify.Remove == fsnotify.Remove {
				// Handle deletion.
				if idx != nil && isMarkdownFile(event.Name) {
					relPath, _ := filepath.Rel(vault, event.Name)
					if err := idx.Delete(relPath); err != nil {
						fmt.Fprintf(os.Stderr, "failed to delete note %q from index: %v\n", relPath, err)
					}
					graphDebounce.Reset(500 * time.Millisecond)
				}
			} else if event.Op&fsnotify.Rename == fsnotify.Rename {
				// For rename: delete old path. The new file will be handled by a subsequent Create event.
				if idx != nil && isMarkdownFile(event.Name) {
					relPath, _ := filepath.Rel(vault, event.Name)
					if err := idx.Delete(relPath); err != nil {
						fmt.Fprintf(os.Stderr, "failed to delete renamed note %q from index: %v\n", relPath, err)
					}
					graphDebounce.Reset(500 * time.Millisecond)
				}
			}

		case err, ok := <-watcher.Errors:
			if !ok {
				return
			}
			fmt.Fprintf(os.Stderr, "watcher error: %v\n", err)

		case <-debounceTimer.C:
			regenCache()
			updateIndexStats()
			savingStats()

		case <-graphDebounce.C:
			buildAndSaveGraph()

		case sig := <-sigChan:
			_ = sig
			// RPC stops first.
			shutCtx, shutCancel := context.WithTimeout(context.Background(), 5*time.Second)
			if err := rpcSrv.Shutdown(shutCtx); err != nil {
				fmt.Fprintf(os.Stderr, "rpc shutdown: %v\n", err)
			}
			shutCancel()
			// Release the graph.
			graphMu.Lock()
			g := currentGraph
			currentGraph = nil
			graphMu.Unlock()
			if g != nil {
				fmt.Fprintf(os.Stderr, "graph: shutdown (%d vertices, %d edges)\n", g.VertexCount(), g.EdgeCount())
			}
			// Close the index last.
			if idx != nil {
				if err := idx.Close(); err != nil {
					fmt.Fprintf(os.Stderr, "failed to close index: %v\n", err)
				}
			}
			os.Remove(pidFile)
			os.Remove(statsFile)
			return
		}
	}
}

// buildGraphFromIndex queries all link rows from the index and builds a CSR graph.
// Returns an empty graph when idx is nil or the query fails.
func buildGraphFromIndex(idx *index.Indexer) *graph.Graph {
	if idx == nil {
		return graph.Build(nil)
	}
	links, err := idx.AllLinks()
	if err != nil {
		fmt.Fprintf(os.Stderr, "graph: failed to load links from index: %v\n", err)
		return graph.Build(nil)
	}
	return graph.Build(links)
}

// writeGraphAtomically writes g to path via a tmp+rename to prevent partial reads.
func writeGraphAtomically(g *graph.Graph, path string) error {
	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		return fmt.Errorf("mkdir: %w", err)
	}
	tmp := path + ".tmp"
	f, err := os.Create(tmp)
	if err != nil {
		return fmt.Errorf("create tmp: %w", err)
	}
	if _, err := g.WriteTo(f); err != nil {
		f.Close()
		os.Remove(tmp)
		return fmt.Errorf("write graph: %w", err)
	}
	if err := f.Sync(); err != nil {
		f.Close()
		os.Remove(tmp)
		return fmt.Errorf("sync tmp: %w", err)
	}
	if err := f.Close(); err != nil {
		os.Remove(tmp)
		return fmt.Errorf("close tmp: %w", err)
	}
	if err := os.Rename(tmp, path); err != nil {
		os.Remove(tmp)
		return fmt.Errorf("rename: %w", err)
	}
	if d, err := os.Open(filepath.Dir(path)); err == nil {
		_ = d.Sync()
		_ = d.Close()
	}
	return nil
}

func isMarkdownFile(path string) bool {
	return strings.HasSuffix(path, ".md")
}

func indexEvent(idx *index.Indexer, vault, filePath string) {
	// Skip non-markdown files.
	if !isMarkdownFile(filePath) {
		return
	}

	relPath, err := filepath.Rel(vault, filePath)
	if err != nil {
		return
	}

	// Read file content.
	content, err := os.ReadFile(filePath)
	if err != nil {
		fmt.Fprintf(os.Stderr, "failed to read file %q: %v\n", filePath, err)
		return
	}

	// Parse markdown to extract wikilinks and title.
	note := vaultpkg.ParseNote(string(content))
	if note == nil {
		return
	}

	// Extract the title (first H1 heading if available, else empty).
	title := ""
	for _, h := range note.Headings {
		if h.Level == 1 {
			title = h.Text
			break
		}
	}

	// Get file modification time.
	fi, err := os.Stat(filePath)
	if err != nil {
		fmt.Fprintf(os.Stderr, "failed to stat file %q: %v\n", filePath, err)
		return
	}

	modTime := fi.ModTime().Unix()

	// Upsert note with links.
	meta := index.NoteMeta{
		Title:   title,
		ModTime: modTime,
	}

	if err := idx.Upsert(relPath, meta, note.Wikilinks); err != nil {
		fmt.Fprintf(os.Stderr, "failed to upsert note %q in index: %v\n", relPath, err)
	}
}

func regenerateCache(vault string, schemas ...vaultpkg.Schema) error {
	schema := vaultpkg.NanikaSchema
	if len(schemas) > 0 {
		schema = schemas[0]
	}
	daily := readDailyNote(vault, time.Now(), schema)
	mocs := readRecentMOCs(vault, time.Now(), schema)
	session := readSessionSnapshot(vault, schema)

	body := formatObsidianBlock(daily, mocs, session)
	if body == "" {
		body = "no context\n"
	}

	return writeAtomically(filepath.Join(vault, ".cache", "preflight.md"), body)
}

type noteRef struct {
	relPath string
	heading string
}

func (n noteRef) render() string {
	if n.heading == "" {
		return fmt.Sprintf(`%s — ""`, n.relPath)
	}
	return fmt.Sprintf(`%s — %q`, n.relPath, n.heading)
}

func readDailyNote(vault string, now time.Time, schemas ...vaultpkg.Schema) noteRef {
	schema := vaultpkg.NanikaSchema
	if len(schemas) > 0 {
		schema = schemas[0]
	}
	dailyDir := filepath.Join(vault, schema.Daily)
	today := now.Format("2006-01-02") + ".md"

	if fi, err := os.Stat(filepath.Join(dailyDir, today)); err == nil && !fi.IsDir() {
		return makeNoteRef(vault, filepath.Join(dailyDir, today))
	}

	entries, err := os.ReadDir(dailyDir)
	if err != nil {
		return noteRef{}
	}

	type cand struct {
		path  string
		mtime time.Time
	}
	var cands []cand
	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), ".md") {
			continue
		}
		info, err := e.Info()
		if err != nil {
			continue
		}
		age := now.Sub(info.ModTime())
		if age < 0 || age > 48*time.Hour {
			continue
		}
		cands = append(cands, cand{
			path:  filepath.Join(dailyDir, e.Name()),
			mtime: info.ModTime(),
		})
	}

	if len(cands) == 0 {
		return noteRef{}
	}

	for i := 0; i < len(cands)-1; i++ {
		for j := i + 1; j < len(cands); j++ {
			if cands[j].mtime.After(cands[i].mtime) {
				cands[i], cands[j] = cands[j], cands[i]
			}
		}
	}

	return makeNoteRef(vault, cands[0].path)
}

func readRecentMOCs(vault string, now time.Time, schemas ...vaultpkg.Schema) []noteRef {
	schema := vaultpkg.NanikaSchema
	if len(schemas) > 0 {
		schema = schemas[0]
	}
	mocsDir := filepath.Join(vault, schema.MOCs)
	entries, err := os.ReadDir(mocsDir)
	if err != nil {
		return nil
	}

	type cand struct {
		path  string
		mtime time.Time
	}
	cands := make([]cand, 0, len(entries))
	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), ".md") {
			continue
		}
		info, err := e.Info()
		if err != nil {
			continue
		}
		age := now.Sub(info.ModTime())
		if age < 0 || age > 48*time.Hour {
			continue
		}
		cands = append(cands, cand{
			path:  filepath.Join(mocsDir, e.Name()),
			mtime: info.ModTime(),
		})
	}

	for i := 0; i < len(cands)-1; i++ {
		for j := i + 1; j < len(cands); j++ {
			if cands[j].mtime.After(cands[i].mtime) ||
				(cands[j].mtime.Equal(cands[i].mtime) && cands[j].path < cands[i].path) {
				cands[i], cands[j] = cands[j], cands[i]
			}
		}
	}

	if len(cands) > 3 {
		cands = cands[:3]
	}

	out := make([]noteRef, 0, len(cands))
	for _, c := range cands {
		out = append(out, makeNoteRef(vault, c.path))
	}
	return out
}

func readSessionSnapshot(vault string, schemas ...vaultpkg.Schema) noteRef {
	schema := vaultpkg.NanikaSchema
	if len(schemas) > 0 {
		schema = schemas[0]
	}
	cwd, err := os.Getwd()
	if err != nil || cwd == "" {
		return noteRef{}
	}

	sessionsDir := filepath.Join(vault, schema.Sessions)
	entries, err := os.ReadDir(sessionsDir)
	if err != nil {
		return noteRef{}
	}

	type cand struct {
		path  string
		mtime time.Time
	}
	var cands []cand
	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), ".md") {
			continue
		}
		p := filepath.Join(sessionsDir, e.Name())
		fmCwd := readFrontmatterCwd(p)
		if fmCwd == "" || fmCwd != cwd {
			continue
		}
		info, err := e.Info()
		if err != nil {
			continue
		}
		cands = append(cands, cand{path: p, mtime: info.ModTime()})
	}

	if len(cands) == 0 {
		return noteRef{}
	}

	for i := 0; i < len(cands)-1; i++ {
		for j := i + 1; j < len(cands); j++ {
			if cands[j].mtime.After(cands[i].mtime) {
				cands[i], cands[j] = cands[j], cands[i]
			}
		}
	}

	return makeNoteRef(vault, cands[0].path)
}

func readFrontmatterCwd(path string) string {
	f, err := os.Open(path)
	if err != nil {
		return ""
	}
	defer f.Close()

	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 0, 8*1024), 64*1024)

	if !scanner.Scan() || strings.TrimSpace(scanner.Text()) != "---" {
		return ""
	}
	for scanner.Scan() {
		line := scanner.Text()
		trimmed := strings.TrimSpace(line)
		if trimmed == "---" {
			return ""
		}
		if strings.HasPrefix(trimmed, "cwd:") {
			v := strings.TrimSpace(strings.TrimPrefix(trimmed, "cwd:"))
			v = strings.Trim(v, `"'`)
			return v
		}
	}
	return ""
}

func makeNoteRef(vault, absPath string) noteRef {
	rel, err := filepath.Rel(vault, absPath)
	if err != nil {
		rel = filepath.Base(absPath)
	}
	return noteRef{relPath: filepath.ToSlash(rel), heading: firstHeading(absPath)}
}

func firstHeading(path string) string {
	f, err := os.Open(path)
	if err != nil {
		return ""
	}
	defer f.Close()

	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 0, 8*1024), 64*1024)

	inFrontmatter := false
	first := true
	bytesRead := 0
	for scanner.Scan() && bytesRead < 4*1024 {
		line := scanner.Text()
		bytesRead += len(line)

		if first {
			first = false
			if strings.TrimSpace(line) == "---" {
				inFrontmatter = true
				continue
			}
		}
		if inFrontmatter {
			if strings.TrimSpace(line) == "---" {
				inFrontmatter = false
			}
			continue
		}
		if strings.HasPrefix(line, "# ") {
			return strings.TrimSpace(strings.TrimPrefix(line, "# "))
		}
	}
	return ""
}

func formatObsidianBlock(daily noteRef, mocs []noteRef, session noteRef) string {
	var parts []string
	if daily.relPath != "" {
		parts = append(parts, "today: "+daily.render())
	}
	if len(mocs) > 0 {
		var sb strings.Builder
		sb.WriteString("mocs (48h):")
		for _, m := range mocs {
			sb.WriteString("\n- ")
			sb.WriteString(m.render())
		}
		parts = append(parts, sb.String())
	}
	if session.relPath != "" {
		parts = append(parts, "session: "+session.render())
	}
	return strings.Join(parts, "\n")
}


func writeAtomically(path string, content string) error {
	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0700); err != nil {
		return fmt.Errorf("mkdir: %w", err)
	}

	tmpPath := path + ".tmp"
	if err := os.WriteFile(tmpPath, []byte(content), 0600); err != nil {
		return fmt.Errorf("write tmp: %w", err)
	}

	if err := os.Rename(tmpPath, path); err != nil {
		os.Remove(tmpPath)
		return fmt.Errorf("rename: %w", err)
	}

	return nil
}

func saveStats(stats *daemonStats) {
	data, err := json.Marshal(stats)
	if err != nil {
		return
	}
	dir := filepath.Dir(statsFile)
	os.MkdirAll(dir, 0700)
	os.WriteFile(statsFile, data, 0600)
}

func stopDaemon() {
	pidBytes, err := os.ReadFile(pidFile)
	if err != nil {
		fmt.Fprintf(os.Stderr, "daemon not running\n")
		os.Exit(1)
	}

	pid, err := strconv.Atoi(strings.TrimSpace(string(pidBytes)))
	if err != nil {
		fmt.Fprintf(os.Stderr, "invalid pid file: %v\n", err)
		os.Exit(1)
	}

	proc, err := os.FindProcess(pid)
	if err != nil {
		fmt.Fprintf(os.Stderr, "daemon not running: %v\n", err)
		os.Exit(1)
	}

	proc.Signal(syscall.SIGTERM)

	started := time.Now()
	for {
		if _, err := os.Stat(pidFile); os.IsNotExist(err) {
			return
		}
		if time.Since(started) > 3*time.Second {
			fmt.Fprintf(os.Stderr, "daemon did not stop after 3 seconds\n")
			os.Exit(1)
		}
		time.Sleep(100 * time.Millisecond)
	}
}

func statusDaemon() {
	data, err := os.ReadFile(statsFile)
	if err != nil {
		fmt.Println("not running")
		os.Exit(1)
	}

	var stats daemonStats
	if err := json.Unmarshal(data, &stats); err != nil {
		fmt.Println("not running")
		os.Exit(1)
	}

	if !processAlive(stats.PID) {
		os.Remove(statsFile)
		os.Remove(pidFile)
		fmt.Println("not running")
		os.Exit(1)
	}

	fmt.Printf("running (pid=%d, started=%s, uptime=%s, regens=%d, last_regen=%s)\n",
		stats.PID,
		stats.StartedAt.Format("2006-01-02 15:04:05"),
		time.Since(stats.StartedAt).Round(time.Second),
		stats.RegenCount,
		stats.LastRegenAt.Format("2006-01-02 15:04:05"),
	)
}

// processAlive reports whether pid corresponds to a live process by sending
// signal 0 — a no-op probe that returns ESRCH for dead PIDs and nil for live
// ones. Required because the daemon can crash without running its SIGTERM
// cleanup, leaving a stale stats.json that would otherwise be reported as
// "running".
func processAlive(pid int) bool {
	if pid <= 0 {
		return false
	}
	proc, err := os.FindProcess(pid)
	if err != nil {
		return false
	}
	return proc.Signal(syscall.Signal(0)) == nil
}

func resolveVault() string {
	if v := os.Getenv("OBSIDIAN_VAULT_PATH"); v != "" {
		return v
	}

	dir := os.Getenv("OBSIDIAN_CONFIG_DIR")
	if dir == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return ""
		}
		dir = filepath.Join(home, ".obsidian")
	}

	f, err := os.Open(filepath.Join(dir, "config"))
	if err != nil {
		return ""
	}
	defer f.Close()

	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 0, 8*1024), 64*1024)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		parts := strings.SplitN(line, "=", 2)
		if len(parts) != 2 {
			continue
		}
		if strings.TrimSpace(parts[0]) == "vault_path" {
			return strings.TrimSpace(parts[1])
		}
	}
	return ""
}

// resolveVaultKind returns the vault kind from the CLI flag, falling back to
// the OBSIDIAN_VAULT_KIND env var. Unknown or absent values default to KindNanika.
func resolveVaultKind(flagVal string) vaultpkg.VaultKind {
	v := flagVal
	if v == "" {
		v = os.Getenv("OBSIDIAN_VAULT_KIND")
	}
	switch v {
	case "nanika":
		return vaultpkg.KindNanika
	case "second-brain":
		return vaultpkg.KindSecondBrain
	default:
		return vaultpkg.KindNanika
	}
}

func resolveSocketPath() string {
	if v := os.Getenv("NANIKA_OBSIDIAN_SOCKET"); v != "" {
		return v
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "/tmp/obsidian.sock"
	}
	return filepath.Join(home, ".alluka", "run", "obsidian.sock")
}

// vault resolver — shared between the obsidian preflight hook and the daemon.
