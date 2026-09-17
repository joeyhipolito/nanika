function projectFeatureRows(snapshot, application) {
  return snapshot.entries.map(function (entry) {
    var applied = null;
    if (entry.name === 'portal-output-cap') {
      applied = application && typeof application.applied === 'boolean' ? application.applied : null;
    }
    return {
      name: entry.name,
      requested: entry.requested === undefined ? null : entry.requested,
      effective: entry.effective,
      applied: applied,
      supported: entry.supported === true,
      source: entry.source,
      reason: entry.reason
    };
  });
}

if (typeof module !== 'undefined' && module.exports) {
  module.exports = { projectFeatureRows: projectFeatureRows };
}
if (typeof globalThis !== 'undefined') {
  globalThis.NanikaFeatureRows = { projectFeatureRows: projectFeatureRows };
}
