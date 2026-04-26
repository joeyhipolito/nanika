package main

import (
	"context"
	"fmt"
	"path/filepath"
	"time"

	"github.com/joeyhipolito/nanika-obsidian/internal/graph"
	"github.com/joeyhipolito/nanika-obsidian/internal/index"
	"github.com/joeyhipolito/nanika-obsidian/internal/recall"
	"github.com/joeyhipolito/nanika-obsidian/internal/rpc"
)

// connectOrFallback attempts to connect to the recall socket, falling back to in-process if enabled.
func connectOrFallback(socketPath, seed string, limit int, timeout time.Duration, noFallback bool, vaultPath string) ([]recall.WalkResult, error) {
	// Try socket connection first if socket path is provided
	if socketPath != "" {
		results, err := queryRecallSocket(socketPath, seed, limit, timeout)
		if err == nil {
			return results, nil
		}
		// If socket fails and no-fallback is set, return the error
		if noFallback {
			return nil, fmt.Errorf("socket unavailable at %s: %w", socketPath, err)
		}
		// Otherwise, fall through to in-process
	}

	// Fallback to in-process recall
	return recallInProcess(vaultPath, seed, limit)
}

// queryRecallSocket connects to and queries the recall RPC socket using the proper RPC protocol.
func queryRecallSocket(socketPath string, seed string, limit int, timeout time.Duration) ([]recall.WalkResult, error) {
	client, err := rpc.Dial(socketPath)
	if err != nil {
		return nil, fmt.Errorf("dial: %w", err)
	}
	defer client.Close()

	// Create context with timeout
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()

	// Query via RPC
	req := rpc.RecallRequest{
		Seed:    seed,
		MaxHops: 2,
		Limit:   limit,
	}
	resp, err := client.Recall(ctx, req)
	if err != nil {
		return nil, fmt.Errorf("recall: %w", err)
	}

	// Convert response paths to WalkResult format
	// Note: scores are not provided by the RPC protocol, so they are zero
	results := make([]recall.WalkResult, len(resp.Paths))
	for i, path := range resp.Paths {
		results[i] = recall.WalkResult{Path: path, Score: 0}
	}

	return results, nil
}

// recallInProcess executes recall using an in-process engine with vault index.
func recallInProcess(vaultPath string, seed string, limit int) ([]recall.WalkResult, error) {
	dbPath := filepath.Join(vaultPath, ".cache", "index.db")

	// Open index from vault cache
	idxr, err := index.OpenIndexer(dbPath)
	if err != nil {
		return nil, fmt.Errorf("open index: %w", err)
	}
	defer idxr.Close()

	// Load links and build graph
	links, err := idxr.AllLinks()
	if err != nil {
		return nil, fmt.Errorf("load links: %w", err)
	}
	g := graph.Build(links)

	// Create and run recall engine
	engine := recall.NewEngine(func() *graph.Graph { return g }, idxr)
	req := recall.Request{
		Seed:    seed,
		MaxHops: 2,
		Limit:   limit,
	}
	results, err := engine.Recall(req)
	if err != nil {
		return nil, fmt.Errorf("engine: %w", err)
	}

	return results, nil
}
