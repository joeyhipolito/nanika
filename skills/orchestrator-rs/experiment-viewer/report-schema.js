(function (root) {
  "use strict";
  const MAX_BYTES = 4 * 1024 * 1024;
  const counters = ["input_tokens", "output_tokens", "cache_read_input_tokens", "cache_creation_input_tokens", "reasoning_output_tokens", "cost_usd"];
  const features = ["barok", "ponytail", "discipline", "portal-output-cap", "portal-review-summary", "kb", "learnings", "worker-memory"];
  function require(value, message) { if (!value) throw new Error(message); }
  function object(value) { return value !== null && typeof value === "object" && !Array.isArray(value); }
  function text(value, limit = 4096) { return typeof value === "string" && value.length <= limit; }
  function number(value) { return Number.isSafeInteger(value) && value >= 0; }
  function nullableNumber(value, money = false) { return value === null || (money ? typeof value === "number" && Number.isFinite(value) && value >= 0 : number(value)); }
  function validate(report) {
    require(object(report), "The report must be a JSON object.");
    require(report.schema === "nanika.portal-experiment.v1", "Unsupported report schema. Open a runner v1 report.");
    require(Object.keys(report).every(key => ["schema", "complete", "manifest", "samples", "summary", "savings_claim", "conclusions", "cache_condition"].includes(key)), "Unexpected report fields; this viewer supports runner v1 only.");
    require(typeof report.complete === "boolean" && report.savings_claim === false && report.conclusions === "descriptive-only" && report.cache_condition === "external/uncontrolled", "The report does not declare the supported evidence limits.");
    const manifest = report.manifest;
    require(object(manifest) && manifest.schema === "nanika.portal-experiment-manifest.v1", "Missing or unsupported manifest.");
    require(number(manifest.pairs) && manifest.pairs >= 2 && manifest.pairs <= 10, "The manifest must plan 2–10 pairs.");
    require(manifest.sample_count === manifest.pairs * 2 && Array.isArray(report.samples) && report.samples.length === manifest.sample_count, "The sample count does not match the planned pairs.");
    require(text(manifest.model, 256) && text(manifest.persona, 256), "Invalid model or persona.");
    require(object(manifest.source) && text(manifest.source.path) && /^[a-f0-9]{40,64}$/.test(manifest.source.head) && /^[a-f0-9]{40,64}$/.test(manifest.source.tree), "Missing source identity.");
    require(Array.isArray(manifest.pins) && manifest.pins.length >= 1 && manifest.pins.length <= 16, "Missing or excessive pinned inputs.");
    for (const pin of manifest.pins) require(object(pin) && text(pin.path) && typeof pin.executable === "boolean" && /^[a-f0-9]{64}$/.test(pin.sha256), "Invalid pinned input.");
    require(manifest.provider_version === null || text(manifest.provider_version, 512), "Invalid provider version.");
    require(manifest.pilot_version === null || text(manifest.pilot_version, 512), "Invalid pilot version.");
    const catalog = manifest.features_catalog;
    require(object(catalog) && catalog.schema === "nanika.feature-catalog.v1" && Array.isArray(catalog.features) && catalog.features.length <= 8 && catalog.features.every(name => features.includes(name)), "Unsupported feature catalog.");
    require(Array.isArray(catalog.capabilities) && catalog.capabilities.length <= 8, "Invalid capability catalog.");
    for (const capability of catalog.capabilities) require(object(capability) && capability.name === "portal-output-cap" && capability.command === "code" && capability.runtime === "codex", "This viewer supports only standalone Codex Portal cap capability.");
    for (let i = 0; i < report.samples.length; i += 1) {
      const sample = report.samples[i];
      const pair = Math.floor(i / 2);
      const arm = pair % 2 === 0 ? ["off", "on"][i % 2] : ["on", "off"][i % 2];
      require(object(sample) && sample.index === i && sample.pair === pair + 1 && sample.arm === arm, "Sample ordering or pair identity is inconsistent.");
      require(["sample_completed", "sample_failed", "not-run"].includes(sample.status) && typeof sample.quality_passed === "boolean", "Invalid sample outcome.");
      require(sample.elapsed_ms === undefined || number(sample.elapsed_ms), "Invalid sample duration.");
      require(sample.reason === undefined || text(sample.reason), "Invalid sample reason.");
      require(sample.identity_drift === undefined || text(sample.identity_drift), "Invalid identity drift reason.");
      if (sample.status !== "sample_completed") { require(!sample.quality_passed, "An unfinished sample cannot pass quality."); continue; }
      require(object(sample.quality_gates) && ["pilot_process", "pilot_completed", "correct_configuration", "observed_application", "verification"].every(key => typeof sample.quality_gates[key] === "boolean"), "Missing quality gates.");
      require(object(sample.pilot_process) && typeof sample.pilot_process.success === "boolean" && object(sample.verification) && typeof sample.verification.success === "boolean", "Missing process or verification receipt.");
      require(!sample.quality_passed || Object.values(sample.quality_gates).every(value => value === true) && sample.pilot_process.success && sample.verification.success, "A passing sample has a failed gate.");
      require(sample.metrics === null || object(sample.metrics), "Invalid usage metrics.");
      if (sample.metrics !== null) {
        require(Object.keys(sample.metrics).every(key => counters.includes(key)), "Unsupported usage counters.");
        for (const key of counters) require(sample.metrics[key] === undefined || nullableNumber(sample.metrics[key], key === "cost_usd"), "Invalid usage counter.");
      }
      require(object(sample.artifacts), "Missing artifact receipts.");
      require(Object.keys(sample.artifacts).length === 4 && Object.keys(sample.artifacts).every(key => ["pilot_result", "features", "usage", "portal_application"].includes(key)), "Unexpected artifact receipts.");
      for (const key of ["pilot_result", "features", "usage", "portal_application"]) {
        const artifact = sample.artifacts[key];
        require(object(artifact) && ["available", "unavailable"].includes(artifact.status) && text(artifact.path), "Invalid artifact reference.");
        if (artifact.status === "unavailable") require(text(artifact.reason) && !Object.hasOwn(artifact, "value"), "Unavailable artifacts cannot contain an authoritative value.");
        else require(Object.hasOwn(artifact, "value"), "Available artifact is missing its value.");
      }
      const snapshot = sample.artifacts.features.value;
      if (snapshot !== undefined && snapshot !== null) {
        require(object(snapshot) && ["nanika.run-features.v1", "nanika.run-features.v2"].includes(snapshot.schema) && Array.isArray(snapshot.entries) && snapshot.entries.length <= 8, "Unsupported feature snapshot.");
        const seen = new Set();
        for (const entry of snapshot.entries) {
          require(object(entry) && features.includes(entry.name) && !seen.has(entry.name), "Invalid or duplicate feature receipt."); seen.add(entry.name);
          require([null, "off", "on"].includes(entry.requested) && ["off", "on"].includes(entry.effective) && typeof entry.supported === "boolean" && text(entry.source, 256) && text(entry.reason, 4096), "Invalid feature setting.");
        }
      }
      const application = sample.artifacts.portal_application.value;
      if (application !== undefined && application !== null) require(object(application) && application.schema === "nanika.portal-application.v1" && typeof application.applied === "boolean" && text(application.status, 256), "Invalid Portal application receipt.");
      if (application?.commands !== undefined) require(Array.isArray(application.commands) && application.commands.length <= 256 && application.commands.every(command => object(command)), "Invalid or excessive Portal commands.");
      if (sample.quality_passed) {
        require(sample.identity_drift === undefined, "A drift-excluded sample cannot pass quality.");
        require(sample.artifacts.features.status === "available" && sample.artifacts.pilot_result.status === "available", "A passing sample requires available authoritative receipts.");
        const cap = snapshot?.entries?.find(entry => entry.name === "portal-output-cap");
        require(cap?.requested === arm && cap?.effective === arm && sample.artifacts.pilot_result.value?.status === "completed", "A passing sample has inconsistent admitted settings or completion.");
        if (arm === "on") require(sample.artifacts.portal_application.status === "available" && application?.applied === true && application.status === "observed" && number(application.verified_wrapped_calls) && application.verified_wrapped_calls > 0, "A passing ON sample lacks observed application.");
      }
    }
    return report;
  }
  const api = { MAX_BYTES, validate };
  root.NanikaReportSchema = api;
  if (typeof module !== "undefined" && module.exports) module.exports = api;
})(globalThis);
