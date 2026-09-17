#!/usr/bin/env node
// Public offline fixture: invented data, never provider evidence or a benchmark.
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { execFileSync } = require('node:child_process');
const viewer = path.join(__dirname, '..', 'skills', 'orchestrator-rs', 'experiment-viewer');
const names = ['barok', 'ponytail', 'discipline', 'portal-output-cap',
  'portal-review-summary', 'kb', 'learnings', 'worker-memory'];
const available = (name, value) => ({path: `/fixture/${name}.json`, status: 'available', value});
const samples = ['off', 'on', 'on', 'off'].map((arm, index) => {
  const passed = index !== 1;
  const application = arm === 'on' ? {
    schema: 'nanika.portal-application.v1', requested: 'on', applied: passed,
    status: passed ? 'observed' : 'no-shell-calls', verified_wrapped_calls: passed ? 1 : 0,
    commands: passed ? [{command_id: 'fixture-command', command_exit_code: 0,
      full_log_bytes: 1000, returned_bytes: 100}] : [],
  } : null;
  const metrics = {input_tokens: 100, output_tokens: 20, cache_read_input_tokens: 0,
    cache_creation_input_tokens: null, reasoning_output_tokens: null, cost_usd: null};
  const features = {schema: 'nanika.run-features.v2', runtime: 'codex', command: 'code',
    entries: names.map(name => ({name, requested: name === 'portal-output-cap' ? arm : null,
      effective: name === 'portal-output-cap' ? arm : 'off', applied: null,
      supported: name === 'portal-output-cap', source: name === 'portal-output-cap'
        ? 'explicit-run-option' : 'runtime-default', reason: 'Synthetic test receipt'}))};
  return {index, pair: Math.floor(index / 2) + 1, arm, status: 'sample_completed',
    quality_passed: passed, elapsed_ms: 1000,
    quality_gates: {pilot_process: true, pilot_completed: true,
      correct_configuration: true, observed_application: passed, verification: true},
    pilot_process: {success: true, elapsed_ms: 900}, verification: {success: true, elapsed_ms: 100},
    metrics, route: {model: 'fixture-model', persona: 'fixture-persona', effort: 'low'},
    provider_tool_failed: false, portal_failed_command_count: arm === 'on' ? 0 : null,
    full_command_log_bytes: arm === 'on' && passed ? 1000 : null,
    captured_provider_stdout_bytes: 200,
    artifacts: {
      pilot_result: available('pilot-result', {status: 'completed', reason: 'Synthetic fixture only'}),
      features: available('run-features', features),
      usage: available('worker-usage', {status: 'available', report: {summary: metrics}}),
      portal_application: application ? available('portal-application', application)
        : {path: '/fixture/portal-application.json', status: 'unavailable', reason: 'OFF mode'},
    }};
});
const report = {
  schema: 'nanika.portal-experiment.v1', complete: true,
  savings_claim: false, conclusions: 'descriptive-only', cache_condition: 'external/uncontrolled',
  manifest: {schema: 'nanika.portal-experiment-manifest.v1', pairs: 2, sample_count: 4,
    model: 'fixture-model', persona: 'fixture-persona',
    source: {path: '/fixture/source', head: '1'.repeat(40), tree: '2'.repeat(40)},
    pins: [{path: '/fixture/pilot', executable: true, sha256: '3'.repeat(64)}],
    provider_version: 'synthetic fixture', pilot_version: null,
    features_catalog: {schema: 'nanika.feature-catalog.v1', features: names,
      capabilities: [{name: 'portal-output-cap', command: 'code', runtime: 'codex'}]}},
  samples, summary: Object.fromEntries(['off', 'on'].map(arm => [arm, {
    planned_count: 2, completed_count: 2, failed_count: 0, not_run_count: 0,
    quality_passed_count: samples.filter(sample => sample.arm === arm && sample.quality_passed).length,
    observed_elapsed_ms_sum: 2000, duration_sample_count: 2,
  }])),
};
const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'nanika-viewer-fixture-'));
try {
  const input = path.join(temporary, 'synthetic-report.json');
  fs.writeFileSync(input, JSON.stringify(report));
  execFileSync(process.execPath, [path.join(viewer, 'feature-rows.test.cjs')]);
  execFileSync(process.execPath, [path.join(viewer, 'report-schema.test.cjs'), input]);
  console.log('Offline synthetic fixture passed: 2 feature projection tests and 14 schema cases; no providers.');
} finally {
  fs.rmSync(temporary, {recursive: true, force: true});
}
