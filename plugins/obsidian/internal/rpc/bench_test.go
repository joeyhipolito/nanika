// Benchmarks for the Obsidian RPC server.
// RED phase: references types that do not exist yet (TRK-530 Phase 3).
package rpc

import (
	"context"
	"fmt"
	"path/filepath"
	"sync"
	"testing"
)

func BenchmarkPing(b *testing.B) {
	dir := b.TempDir()
	sock := filepath.Join(dir, "bench.sock")
	srv := New(Config{Store: nil, Graph: nil})
	if err := srv.Start(sock); err != nil {
		b.Fatalf("Start: %v", err)
	}
	b.Cleanup(func() { _ = srv.Shutdown(context.Background()) })

	c, err := Dial(sock)
	if err != nil {
		b.Fatalf("Dial: %v", err)
	}
	b.Cleanup(func() { _ = c.Close() })

	ctx := context.Background()
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		if err := c.Ping(ctx); err != nil {
			b.Fatalf("Ping: %v", err)
		}
	}
}

func BenchmarkRecall_Stub(b *testing.B) {
	dir := b.TempDir()
	sock := filepath.Join(dir, "bench.sock")
	srv := New(Config{Store: nil, Graph: nil})
	if err := srv.Start(sock); err != nil {
		b.Fatalf("Start: %v", err)
	}
	b.Cleanup(func() { _ = srv.Shutdown(context.Background()) })

	c, err := Dial(sock)
	if err != nil {
		b.Fatalf("Dial: %v", err)
	}
	b.Cleanup(func() { _ = c.Close() })

	req := RecallRequest{Seed: "notes/index.md", MaxHops: 2, Limit: 20}
	ctx := context.Background()
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		if _, err := c.Recall(ctx, req); err != nil {
			b.Fatalf("Recall: %v", err)
		}
	}
}

func BenchmarkConcurrent_50(b *testing.B) {
	dir := b.TempDir()
	sock := filepath.Join(dir, "bench.sock")
	srv := New(Config{Store: nil, Graph: nil})
	if err := srv.Start(sock); err != nil {
		b.Fatalf("Start: %v", err)
	}
	b.Cleanup(func() { _ = srv.Shutdown(context.Background()) })

	const workers = 50
	clients := make([]*Client, workers)
	for i := 0; i < workers; i++ {
		c, err := Dial(sock)
		if err != nil {
			b.Fatalf("Dial worker %d: %v", i, err)
		}
		clients[i] = c
	}
	b.Cleanup(func() {
		for _, c := range clients {
			_ = c.Close()
		}
	})

	ctx := context.Background()
	b.ResetTimer()
	b.RunParallel(func(pb *testing.PB) {
		var wg sync.WaitGroup
		sema := make(chan struct{}, workers)
		i := 0
		for pb.Next() {
			sema <- struct{}{}
			wg.Add(1)
			go func(idx int) {
				defer func() { <-sema; wg.Done() }()
				c := clients[idx%workers]
				req := RecallRequest{
					Seed:    fmt.Sprintf("notes/node-%d.md", idx%20),
					MaxHops: 2,
					Limit:   10,
				}
				if _, err := c.Recall(ctx, req); err != nil {
					b.Errorf("Recall worker %d: %v", idx, err)
				}
			}(i)
			i++
		}
		wg.Wait()
	})
}
