package learning

import (
	"sync"

	"github.com/joeyhipolito/orchestrator-cli/internal/event"
)

var (
	emitterMu  sync.RWMutex
	pkgEmitter event.Emitter = event.NoOpEmitter{}
)

// SetEmitter configures the package-level emitter used for learning events.
// Call once at startup before any capture; safe for concurrent reads after that.
func SetEmitter(e event.Emitter) {
	emitterMu.Lock()
	pkgEmitter = e
	emitterMu.Unlock()
}

func getEmitter() event.Emitter {
	emitterMu.RLock()
	e := pkgEmitter
	emitterMu.RUnlock()
	return e
}
