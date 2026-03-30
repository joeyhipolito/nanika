/**
 * Promptfoo custom grading provider: routes llm-rubric judge calls through Claude CLI.
 * Used as defaultTest.options.provider so that llm-rubric assertions don't require
 * OPENAI_API_KEY or ANTHROPIC_API_KEY.
 */
import { execFileSync } from 'child_process';

export default class ClaudeCliGrader {
  constructor(options = {}) {
    this.id = () => 'claude-cli-grader';
    this._options = options;
  }

  async callApi(prompt) {
    try {
      const output = execFileSync(
        'claude',
        [
          '--model', 'claude-sonnet-4-5',
          '--print',
          '--output-format', 'text',
          '--max-turns', '1',
          '--dangerously-skip-permissions',
          prompt,
        ],
        {
          encoding: 'utf8',
          timeout: 120000,
          maxBuffer: 1024 * 1024,
        },
      );
      return { output: output.trim() };
    } catch (err) {
      const msg = err.stdout || err.stderr || err.message;
      return { error: String(msg) };
    }
  }
}
