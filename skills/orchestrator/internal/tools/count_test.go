package tools

import (
	"context"
	"fmt"
	"testing"
)

func TestRegistry_Load_Count(t *testing.T) {
	// Use real nanika plugins dir
	r := Load(context.Background())
	all := r.All()
	fmt.Printf("\nTotal tools: %d\n", len(all))
	for _, tool := range all {
		fmt.Printf("  [%s] %s\n", tool.Risk(), tool.Name())
	}
	if len(all) < 10 { // tier1(6) + tier3(4)
		t.Errorf("expected >=10 tools on any install, got %d", len(all))
	}
}
