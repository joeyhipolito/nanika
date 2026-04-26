package graph

import (
	"slices"

	"github.com/joeyhipolito/nanika-obsidian/internal/index"
)

// Graph represents a directed graph of note links using Compressed Sparse Row format.
// Nodes are strings (file paths), and edges represent one-way links.
// Graph is immutable; use Build to create a new instance.
type Graph struct {
	nodes     map[string]int // node name → node ID
	nodeNames []string       // node ID → node name (indexed by ID)
	rowPtr    []int          // CSR row pointers: rowPtr[id] is the start index in col for node id
	col       []int          // CSR column indices: col[rowPtr[id]:rowPtr[id+1]] are the adjacent nodes for node id
}

// Build constructs a Graph from a slice of LinkRows.
// Self-loops and links with empty Src or Dst are silently dropped.
// Returns a non-nil Graph even if the input is empty or all links are invalid.
func Build(links []index.LinkRow) *Graph {
	g := &Graph{
		nodes:     make(map[string]int),
		nodeNames: []string{},
		rowPtr:    []int{0},
		col:       []int{},
	}

	if len(links) == 0 {
		return g
	}

	// First pass: collect all unique nodes.
	nodeSet := make(map[string]bool)
	for _, link := range links {
		if link.Src != "" {
			nodeSet[link.Src] = true
		}
		if link.Dst != "" {
			nodeSet[link.Dst] = true
		}
	}

	// Assign IDs to nodes in sorted order for deterministic output.
	nodeList := make([]string, 0, len(nodeSet))
	for node := range nodeSet {
		nodeList = append(nodeList, node)
	}
	slices.Sort(nodeList)

	for id, node := range nodeList {
		g.nodes[node] = id
		g.nodeNames = append(g.nodeNames, node)
	}

	// Second pass: build adjacency lists for each node.
	adjLists := make([][]int, len(nodeList))
	for _, link := range links {
		// Skip self-loops and empty edges.
		if link.Src == "" || link.Dst == "" || link.Src == link.Dst {
			continue
		}

		srcID := g.nodes[link.Src]
		dstID := g.nodes[link.Dst]

		adjLists[srcID] = append(adjLists[srcID], dstID)
	}

	// Sort adjacency lists and build CSR format.
	for _, adj := range adjLists {
		slices.Sort(adj)
		g.col = append(g.col, adj...)
		g.rowPtr = append(g.rowPtr, len(g.col))
	}

	return g
}

// Neighbours returns a sorted slice of nodes that this node links to.
// Returns nil if the node is not in the graph.
// Returns a non-nil slice (possibly empty) if the node is in the graph.
func (g *Graph) Neighbours(node string) []string {
	id, ok := g.nodes[node]
	if !ok {
		return nil
	}

	start := g.rowPtr[id]
	end := g.rowPtr[id+1]

	// Return the sorted node names. If no edges, return non-nil empty slice.
	result := make([]string, 0, end-start)
	for _, colID := range g.col[start:end] {
		result = append(result, g.nodeNames[colID])
	}
	return result
}

// BFS performs a breadth-first search starting from the seed node,
// returning all nodes reachable within maxHops steps (excluding the seed).
// Returns nil if the seed is not in the graph.
// Results are sorted lexicographically.
func (g *Graph) BFS(seed string, maxHops int) []string {
	_, ok := g.nodes[seed]
	if !ok {
		return nil
	}

	visited := make(map[string]bool)
	visited[seed] = true

	currentLevel := []string{seed}
	var result []string

	for hop := 0; hop < maxHops && len(currentLevel) > 0; hop++ {
		var nextLevel []string

		for _, node := range currentLevel {
			neighbours := g.Neighbours(node)
			for _, neighbour := range neighbours {
				if !visited[neighbour] {
					visited[neighbour] = true
					result = append(result, neighbour)
					nextLevel = append(nextLevel, neighbour)
				}
			}
		}

		currentLevel = nextLevel
	}

	slices.Sort(result)
	return result
}

// VertexCount returns the number of nodes in the graph.
func (g *Graph) VertexCount() int {
	return len(g.nodeNames)
}

// EdgeCount returns the number of edges in the graph.
func (g *Graph) EdgeCount() int {
	return len(g.col)
}

