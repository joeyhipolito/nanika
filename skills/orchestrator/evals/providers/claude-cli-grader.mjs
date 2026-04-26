/**
 * Promptfoo custom grading provider: routes llm-rubric judge calls through Claude CLI.
 * Used as defaultTest.options.provider so that llm-rubric assertions don't require
 * OPENAI_API_KEY or ANTHROPIC_API_KEY.
 *
 * GOTCHA: This grader and the subject provider (claude-cli.mjs) both draw from the
 * same Claude CLI daily quota pool. On a 12-test run the two providers together can
 * fire 24+ CLI invocations. Keep evaluateOptions.maxConcurrency ≤ 2 in the YAML to
 * avoid exhausting the cap mid-run (which causes grader failures that look like eval
 * failures). The RATE_LIMIT_MARKER below turns exhaustion into a structured error
 * rather than an opaque exception, so promptfoo marks the test errored (not failed).
 */
import { execFileSync } from 'child_process';

const RATE_LIMIT_MARKER = "You've hit your limit";

export default class ClaudeCliGrader {
  constructor(options = {}) {
    this.id = () => 'claude-cli-grader';
    this._options = options;
  }

  async callApi(prompt) {
    // Append explicit format instruction so Claude always opens with Pass/Fail.
    // Without this, Claude writes prose that promptfoo can't parse and marks as fail.
    const gradingPrompt = `${prompt}\n\nRespond with exactly "Pass" or "Fail" on the first line, then your reasoning.`;
    try {
      const output = execFileSync(
        'claude',
        [
          '--model', 'claude-sonnet-4-6',
          '--print',
          '--output-format', 'text',
          '--max-turns', '1',
          '--dangerously-skip-permissions',
          gradingPrompt,
        ],
        {
          encoding: 'utf8',
          timeout: 120000,
          maxBuffer: 1024 * 1024,
        },
      );
      const trimmed = output.trim();
      if (trimmed.includes(RATE_LIMIT_MARKER)) {
        return { error: 'rate_limit: Claude CLI daily cap exhausted — re-run after the reset or reduce evaluateOptions.maxConcurrency' };
      }
      return { output: trimmed };
    } catch (err) {
      const msg = err.stdout || err.stderr || err.message || String(err);
      if (msg.includes(RATE_LIMIT_MARKER)) {
        return { error: 'rate_limit: Claude CLI daily cap exhausted — re-run after the reset or reduce evaluateOptions.maxConcurrency' };
      }
      return { error: String(msg) };
    }
  }
}
