(function () {
  "use strict";
  const $ = id => document.getElementById(id);
  const formatter = new Intl.NumberFormat("en");
  let report = null;
  let generation = 0;
  const safeText = value => typeof value === "string" ? value.slice(0, 4096) : "Unknown";
  const count = value => Number.isSafeInteger(value) && value >= 0 ? formatter.format(value) : "Unknown";
  const seconds = value => Number.isSafeInteger(value) && value >= 0 ? `${(value / 1000).toFixed(2)} s` : "Unknown";
  const yesNo = value => typeof value === "boolean" ? value ? "Yes" : "No" : "Not observed";
  function node(tag, text, className) { const item = document.createElement(tag); if (text !== undefined) item.textContent = text; if (className) item.className = className; return item; }
  function cell(row, text, className) { row.append(node("td", text, className)); }
  function definition(list, term, value) { list.append(node("dt", term), node("dd", value)); }
  function table(headers, rows, label) {
    const wrap = node("div", undefined, "table-scroll");
    wrap.tabIndex = 0; wrap.setAttribute("role", "region"); wrap.setAttribute("aria-label", label);
    const element = node("table"); const head = node("thead"); const header = node("tr");
    for (const text of headers) { const th = node("th", text); th.scope = "col"; header.append(th); }
    head.append(header); element.append(head); const body = node("tbody");
    for (const values of rows) { const row = node("tr"); for (const value of values) cell(row, value); body.append(row); }
    element.append(body); wrap.append(element); return wrap;
  }
  function renderTable() {
    if (!report) return;
    const selected = report.samples.filter(sample => ($("arm").value === "all" || sample.arm === $("arm").value) && ($("quality").value === "all" || sample.quality_passed === ($("quality").value === "pass")));
    $("samples").replaceChildren();
    for (const sample of selected) {
      const row = node("tr"); const metrics = sample.metrics || {};
      cell(row, `${sample.pair} / ${sample.index + 1}`); cell(row, sample.arm.toUpperCase(), "arm");
      cell(row, sample.status === "not-run" ? "Not run" : sample.quality_passed ? "Passed" : "Did not pass", sample.quality_passed ? "pass" : "fail");
      cell(row, seconds(sample.elapsed_ms), "numeric");
      for (const key of ["input_tokens", "output_tokens", "cache_read_input_tokens"]) cell(row, count(metrics[key]), "numeric");
      cell(row, typeof metrics.cost_usd === "number" ? `$${metrics.cost_usd.toFixed(4)}` : "Unknown", "numeric");
      const evidence = node("td"); const link = node("a", "Receipts"); link.href = `#sample-${sample.index}`;
      link.addEventListener("click", () => { const details = $(`sample-${sample.index}`); details.open = true; }); evidence.append(link); row.append(evidence);
      $("samples").append(row);
    }
    $("visible-count").textContent = `${selected.length} of ${report.samples.length} attempts shown`;
    $("no-matches").hidden = selected.length !== 0;
  }
  function renderReceipt(sample) {
    const details = node("details"); details.id = `sample-${sample.index}`;
    const summary = node("summary", `Pair ${sample.pair} · attempt ${sample.index + 1} · ${sample.arm.toUpperCase()} — ${sample.quality_passed ? "quality passed" : sample.status === "not-run" ? "not run" : "quality did not pass"}`);
    details.append(summary); const body = node("div", undefined, "receipt-body"); details.append(body);
    if (sample.status !== "sample_completed") { body.append(node("p", safeText(sample.reason), "reason")); return details; }
    const gates = node("ul", undefined, "gate-list");
    const names = {pilot_process:"Pilot process",pilot_completed:"Pilot completion",correct_configuration:"Arm configuration",observed_application:"Application gate",verification:"Code verification"};
    for (const [key, label] of Object.entries(names)) gates.append(node("li", `${label}: ${sample.quality_gates[key] ? "passed" : "failed"}`, sample.quality_gates[key] ? "pass" : "fail"));
    body.append(gates);
    const artifacts = sample.artifacts; const pilot = artifacts.pilot_result.status === "available" ? artifacts.pilot_result.value || {} : {}; const application = artifacts.portal_application.status === "available" ? artifacts.portal_application.value : undefined;
    body.append(node("p", safeText(sample.identity_drift || application?.reason || pilot.reason || "No additional outcome note."), "reason"));
    const observations = node("dl");
    definition(observations, "Pilot / verifier elapsed", `${seconds(sample.pilot_process.elapsed_ms)} / ${seconds(sample.verification.elapsed_ms)}`);
    definition(observations, "Provider reported failed tool", yesNo(sample.provider_tool_failed));
    definition(observations, "Verified failed Portal commands", count(sample.portal_failed_command_count));
    definition(observations, "Full command log bytes", count(sample.full_command_log_bytes));
    definition(observations, "Captured provider stdout bytes", count(sample.captured_provider_stdout_bytes));
    definition(observations, "Cache write / reasoning tokens", `${count(sample.metrics?.cache_creation_input_tokens)} / ${count(sample.metrics?.reasoning_output_tokens)}`);
    definition(observations, "Observed route", [sample.route?.persona,sample.route?.model,sample.route?.effort].map(safeText).join(" / "));
    body.append(observations, node("h3", "Requested, effective and observed"));
    const snapshot = artifacts.features.status === "available" ? artifacts.features.value : undefined;
    if (snapshot?.entries) {
      const rows = NanikaFeatureRows.projectFeatureRows(snapshot, application);
      body.append(table(["Feature", "Requested", "Effective", "Observed applied", "Supported", "Source"], rows.map(row => [row.name,row.requested === null ? "Default" : row.requested.toUpperCase(),row.effective.toUpperCase(),yesNo(row.applied),row.supported ? "Yes" : "No",row.source]), "Feature receipts, scroll horizontally on smaller screens"));
      body.append(node("p", "Unsupported features are not activated. An admitted ON setting does not prove observed application.", "note"));
    } else body.append(node("p", "Feature receipt unavailable.", "note"));
    if (Array.isArray(application?.commands)) {
      body.append(node("h3", "Observed wrapper commands"));
      body.append(table(["Command", "Exit code", "Full log bytes", "Returned bytes"], application.commands.slice(0,256).map(command => [safeText(command.command_id),Number.isSafeInteger(command.command_exit_code) ? String(command.command_exit_code) : "Unknown",count(command.full_log_bytes),count(command.returned_bytes)]), "Observed wrapper commands, scroll horizontally on smaller screens"));
    }
    for (const [name, artifact] of Object.entries(artifacts)) if (artifact.status === "unavailable") body.append(node("p", `${name}: unavailable — ${safeText(artifact.reason)}`, "note"));
    return details;
  }
  function render() {
    const manifest = report.manifest;
    $("outcome-title").textContent = `${report.samples.filter(sample => sample.quality_passed).length} of ${report.samples.length} samples passed quality`;
    $("identity").textContent = `${manifest.model} · ${manifest.persona} · ${manifest.pairs} matched pairs · source ${manifest.source.head.slice(0,12)} · ${report.complete ? "finished" : "partial report"}`;
    $("capability").textContent = manifest.features_catalog.capabilities.length ? "Portal output cap is recorded as supported for standalone Codex code. Application is checked separately in each attempt." : "No supported ON capability is recorded in this catalog.";
    $("receipts").replaceChildren(...report.samples.map(renderReceipt));
    const pins = $("pins"); pins.replaceChildren();
    definition(pins,"Source repository",manifest.source.path); definition(pins,"Source commit",manifest.source.head); definition(pins,"Source tree",manifest.source.tree);
    definition(pins,"Provider version",manifest.provider_version || "Unknown"); definition(pins,"Pilot version",manifest.pilot_version || "Unknown — identified by SHA256 and catalog");
    for (const pin of manifest.pins) definition(pins,pin.path,pin.sha256);
    $("empty").hidden = true; $("loaded").hidden = false; renderTable();
  }
  $("report-file").addEventListener("change", async event => {
    const current = ++generation; report = null; $("loaded").hidden = true; $("empty").hidden = false; $("error").hidden = true;
    const file = event.target.files[0]; if (!file) return;
    try {
      if (file.size > NanikaReportSchema.MAX_BYTES) throw new Error("This file exceeds 4 MiB. Choose a bounded runner report.");
      const contents = await file.text(); if (current !== generation) return;
      report = NanikaReportSchema.validate(JSON.parse(contents)); $("arm").value = "all"; $("quality").value = "all"; render();
    } catch (error) { if (current !== generation) return; report = null; $("loaded").hidden = true; $("empty").hidden = false; $("error").textContent = `Could not open report: ${error instanceof SyntaxError ? "invalid JSON. Choose the runner's report.json file." : safeText(error.message)}`; $("error").hidden = false; }
  });
  $("arm").addEventListener("change", renderTable); $("quality").addEventListener("change", renderTable);
})();
