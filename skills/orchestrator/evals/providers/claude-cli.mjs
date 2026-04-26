/**
 * Promptfoo custom provider: routes eval prompts through the Claude Code CLI.
 * This matches how the orchestrator calls the LLM internally (via sdk.QueryText),
 * so the eval uses the same model and auth path as production.
 *
 * Usage in decomposer.yaml:
 *   providers:
 *     - file://providers/claude-cli.mjs
 */
import { execFileSync } from 'child_process';

export default class ClaudeCliProvider {
  constructor(options = {}) {
    this.id = () => 'claude-cli';
    this._options = options;
  }

  /**
   * @param {string} prompt - The fully-rendered prompt string from promptfoo.
   * @returns {{ output: string } | { error: string }}
   */
  async callApi(prompt) {
    try {
      const output = execFileSync(
        'claude',
        [
          '--model', 'claude-sonnet-4-6',
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
