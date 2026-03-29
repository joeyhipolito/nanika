package routing

import "strings"

// TaskType classifies the nature of a task for cross-target phase-shape lookup.
// Values are stored in phase_shape_patterns.task_type and must be DB-stable:
// changing a constant requires a data migration.
type TaskType string

const (
	TaskTypeImplementation TaskType = "implementation"
	TaskTypeBugfix         TaskType = "bugfix"
	TaskTypeRefactor       TaskType = "refactor"
	TaskTypeResearch       TaskType = "research"
	TaskTypeDeployment     TaskType = "deployment"
	TaskTypeDocs           TaskType = "docs"
	TaskTypeTest           TaskType = "test"
	TaskTypeWriting        TaskType = "writing"
	TaskTypeUnknown        TaskType = "unknown"
)

// taskTypeRule associates a TaskType with a list of keyword substrings.
// The first rule whose any keyword appears in the lowercased task wins.
// Order encodes priority: more specific rules come before broader ones.
type taskTypeRule struct {
	taskType TaskType
	keywords []string
}

// taskTypeRules is the ordered priority list used by ClassifyTaskType.
// Rules are checked top-to-bottom; the first match wins.
//
// Priority rationale:
//   - bugfix before implementation: "fix" is more specific than "add/build".
//   - deployment before implementation: "deploy/release" is more specific than "build".
//   - test before implementation: "test" is narrower than "write/add".
//   - refactor before research: "refactor" beats the generic "review" signal.
//   - docs before research: documentation tasks are explicit.
//   - writing before implementation: publication/content tasks must not fall through
//     to implementation via the broad "write" keyword. Keywords are compound phrases
//     or domain-specific nouns unambiguous in content-creation context.
//   - research is broad ("review", "investigate") so it comes late.
//   - implementation is the catch-all and comes last.
var taskTypeRules = []taskTypeRule{
	{TaskTypeBugfix, []string{"fix ", "bug", "debug", "repair", "broken", "failing", "crash", " error"}},
	{TaskTypeDeployment, []string{"deploy", "release ", "rollout", "migration", "migrate"}},
	{TaskTypeTest, []string{"test", "testing", "coverage", "benchmark", "fuzz"}},
	{TaskTypeRefactor, []string{"refactor", "clean up", "cleanup", "restructure", "reorganize", "simplify"}},
	{TaskTypeDocs, []string{"document", "documentation", "readme", "changelog", " spec ", "design doc"}},
	{TaskTypeWriting, []string{"blog post", "newsletter", "substack", " article", "write blog", "draft post", "draft blog", "content creation", "narration", "linkedin post", "reddit post", "publish post"}},
	{TaskTypeResearch, []string{"research", "investigate", "analyze", "analyse", "explore", "audit", "review", "study"}},
	{TaskTypeImplementation, []string{"implement", "add ", "build", "create", "write", "develop", " code"}},
}

// ClassifyTaskType returns the task type for the given task text.
// Classification uses substring matching on the lowercased text; the first
// matching rule wins. Returns TaskTypeUnknown when no rule matches.
//
// The result is advisory: callers use it for cross-target shape lookup
// and must not rely on it being correct for every task.
func ClassifyTaskType(task string) TaskType {
	if task == "" {
		return TaskTypeUnknown
	}
	lower := strings.ToLower(task)
	for _, rule := range taskTypeRules {
		for _, kw := range rule.keywords {
			if strings.Contains(lower, kw) {
				return rule.taskType
			}
		}
	}
	return TaskTypeUnknown
}
