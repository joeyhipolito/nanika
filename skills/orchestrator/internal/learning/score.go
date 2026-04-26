package learning

// HeuristicScore returns the base-tier quality score for a learning type.
// Values mirror scripts/learnings-rescore.sql lines 12-22 so that live-captured
// learnings receive the same base tier the backfill rescore would assign.
func HeuristicScore(t LearningType) float64 {
	switch t {
	case "insight":
		return 1.0
	case "decision":
		return 0.8
	case "pattern":
		return 0.7
	case "error":
		return 0.6
	case "source":
		return 0.4
	case "preference":
		return 0.3
	case "behavior":
		return 0.3
	default:
		return 0.5
	}
}
