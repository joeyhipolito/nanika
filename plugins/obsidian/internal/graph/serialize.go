package graph

import (
	"encoding/binary"
	"fmt"
	"hash/crc32"
	"io"
)

const (
	magic   = "CSR1"
	version = 1
)

// WriteTo serializes the graph to a writer using CSR1 format.
// Format:
//   [4 bytes] magic "CSR1"
//   [1 byte] version
//   [4 bytes] node count
//   [for each node] [4 bytes] name length + [variable] name bytes
//   [4 bytes] row pointer count (node count + 1)
//   [for each row pointer] [4 bytes] row pointer value
//   [4 bytes] edge count
//   [for each edge] [4 bytes] column index (target node ID)
//   [4 bytes] CRC32 checksum of all preceding data
//
// Returns an error if writing fails at any point.
// Errors are wrapped with context.
func (g *Graph) WriteTo(w io.Writer) (int64, error) {
	// Create a buffer to compute CRC32 over the entire payload except the CRC itself.
	payload := &crcWriter{w: w}
	var totalWritten int64

	// Write magic header.
	if n, err := payload.Write([]byte(magic)); err != nil {
		return totalWritten + int64(n), fmt.Errorf("writing magic: %w", err)
	} else {
		totalWritten += int64(n)
	}

	// Write version.
	versionBuf := []byte{version}
	if n, err := payload.Write(versionBuf); err != nil {
		return totalWritten + int64(n), fmt.Errorf("writing version: %w", err)
	} else {
		totalWritten += int64(n)
	}

	// Write node count.
	if err := binary.Write(payload, binary.LittleEndian, int32(len(g.nodeNames))); err != nil {
		return totalWritten, fmt.Errorf("writing node count: %w", err)
	}
	totalWritten += 4

	// Write each node name.
	for _, name := range g.nodeNames {
		if err := binary.Write(payload, binary.LittleEndian, int32(len(name))); err != nil {
			return totalWritten, fmt.Errorf("writing node name length: %w", err)
		}
		totalWritten += 4

		if n, err := payload.Write([]byte(name)); err != nil {
			return totalWritten + int64(n), fmt.Errorf("writing node name: %w", err)
		} else {
			totalWritten += int64(n)
		}
	}

	// Write row pointer count.
	if err := binary.Write(payload, binary.LittleEndian, int32(len(g.rowPtr))); err != nil {
		return totalWritten, fmt.Errorf("writing row pointer count: %w", err)
	}
	totalWritten += 4

	// Write each row pointer.
	for _, ptr := range g.rowPtr {
		if err := binary.Write(payload, binary.LittleEndian, int32(ptr)); err != nil {
			return totalWritten, fmt.Errorf("writing row pointer: %w", err)
		}
		totalWritten += 4
	}

	// Write edge count.
	if err := binary.Write(payload, binary.LittleEndian, int32(len(g.col))); err != nil {
		return totalWritten, fmt.Errorf("writing edge count: %w", err)
	}
	totalWritten += 4

	// Write each edge (column index).
	for _, col := range g.col {
		if err := binary.Write(payload, binary.LittleEndian, int32(col)); err != nil {
			return totalWritten, fmt.Errorf("writing column index: %w", err)
		}
		totalWritten += 4
	}

	// Compute and write CRC32 of the payload so far.
	crc := payload.sum32()
	if err := binary.Write(w, binary.LittleEndian, crc); err != nil {
		return totalWritten, fmt.Errorf("writing CRC32: %w", err)
	}
	totalWritten += 4

	return totalWritten, nil
}

// Load deserializes a graph from a reader using CSR1 format.
// Validates the magic header, version, and CRC32 checksum.
// Returns an error if the format is invalid or the data is corrupted.
func Load(r io.Reader) (*Graph, error) {
	// Read the entire stream into memory so we can compute CRC32.
	payload, err := io.ReadAll(r)
	if err != nil {
		return nil, fmt.Errorf("reading payload: %w", err)
	}

	if len(payload) < 4+1+4+4 {
		// Minimum: magic (4) + version (1) + node count (4) + edge count (4) + CRC (4) = 18 bytes
		return nil, fmt.Errorf("payload too short: %d bytes", len(payload))
	}

	// Split off the CRC from the payload.
	crc32Stored := binary.LittleEndian.Uint32(payload[len(payload)-4:])
	payloadData := payload[:len(payload)-4]

	// Verify magic.
	if string(payloadData[:4]) != magic {
		return nil, fmt.Errorf("invalid magic: expected %q, got %q", magic, string(payloadData[:4]))
	}

	// Verify CRC32.
	crc32Computed := crc32.ChecksumIEEE(payloadData)
	if crc32Computed != crc32Stored {
		return nil, fmt.Errorf("CRC32 mismatch: expected %08x, got %08x", crc32Stored, crc32Computed)
	}

	// Create a reader for the payload (without the CRC).
	br := &bufReader{buf: payloadData, offset: 0}

	// Read magic (already verified above, but skip it anyway).
	_ = make([]byte, 4)
	if _, err := br.Read(make([]byte, 4)); err != nil {
		return nil, fmt.Errorf("reading magic: %w", err)
	}

	// Read version.
	vBuf := make([]byte, 1)
	if _, err := br.Read(vBuf); err != nil {
		return nil, fmt.Errorf("reading version: %w", err)
	}
	if vBuf[0] != version {
		return nil, fmt.Errorf("unsupported version: %d", vBuf[0])
	}

	// Read node count.
	var nodeCount int32
	if err := binary.Read(br, binary.LittleEndian, &nodeCount); err != nil {
		return nil, fmt.Errorf("reading node count: %w", err)
	}

	// Read node names.
	nodeNames := make([]string, nodeCount)
	for i := range nodeCount {
		var nameLen int32
		if err := binary.Read(br, binary.LittleEndian, &nameLen); err != nil {
			return nil, fmt.Errorf("reading node name length: %w", err)
		}

		nameBuf := make([]byte, nameLen)
		if _, err := br.Read(nameBuf); err != nil {
			return nil, fmt.Errorf("reading node name: %w", err)
		}
		nodeNames[i] = string(nameBuf)
	}

	// Read row pointer count.
	var rowPtrCount int32
	if err := binary.Read(br, binary.LittleEndian, &rowPtrCount); err != nil {
		return nil, fmt.Errorf("reading row pointer count: %w", err)
	}

	// Read row pointers.
	rowPtr := make([]int, rowPtrCount)
	for i := range rowPtrCount {
		var ptr int32
		if err := binary.Read(br, binary.LittleEndian, &ptr); err != nil {
			return nil, fmt.Errorf("reading row pointer: %w", err)
		}
		rowPtr[i] = int(ptr)
	}

	// Read edge count.
	var edgeCount int32
	if err := binary.Read(br, binary.LittleEndian, &edgeCount); err != nil {
		return nil, fmt.Errorf("reading edge count: %w", err)
	}

	// Read edges (column indices).
	col := make([]int, edgeCount)
	for i := range edgeCount {
		var c int32
		if err := binary.Read(br, binary.LittleEndian, &c); err != nil {
			return nil, fmt.Errorf("reading column index: %w", err)
		}
		col[i] = int(c)
	}

	// Reconstruct the graph.
	g := &Graph{
		nodes:     make(map[string]int),
		nodeNames: nodeNames,
		rowPtr:    rowPtr,
		col:       col,
	}

	// Rebuild the nodes map from nodeNames.
	for id, name := range nodeNames {
		g.nodes[name] = id
	}

	return g, nil
}

// crcWriter wraps an io.Writer and computes CRC32 of all written data.
type crcWriter struct {
	w   io.Writer
	crc uint32
}

func (c *crcWriter) Write(p []byte) (int, error) {
	n, err := c.w.Write(p)
	if err != nil {
		return n, err
	}
	c.crc = crc32.Update(c.crc, crc32.IEEETable, p[:n])
	return n, nil
}

func (c *crcWriter) sum32() uint32 {
	return c.crc
}

// bufReader is a simple reader over a byte slice.
type bufReader struct {
	buf    []byte
	offset int
}

func (b *bufReader) Read(p []byte) (int, error) {
	if b.offset >= len(b.buf) {
		return 0, io.EOF
	}
	n := copy(p, b.buf[b.offset:])
	b.offset += n
	return n, nil
}
