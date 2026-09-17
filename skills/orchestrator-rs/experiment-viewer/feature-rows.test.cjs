const assert = require('node:assert');
const { projectFeatureRows } = require('./feature-rows.js');

// capON snapshot.applied null + application false stays false; unsupported feature remains null
{
  const snapshot = {
    entries: [
      { name: 'portal-output-cap', requested: true, effective: true, applied: null, supported: true, source: 'config', reason: null },
      { name: 'unsupported-feature', requested: true, effective: false, applied: null, supported: false, source: 'config', reason: 'unsupported' }
    ]
  };
  const application = { applied: false };
  const rows = projectFeatureRows(snapshot, application);
  assert.strictEqual(rows[0].applied, false);
  assert.strictEqual(rows[1].applied, null);
}

// missing application keeps null; explicitly requested OFF remains OFF
{
  const snapshot = {
    entries: [
      { name: 'portal-output-cap', requested: false, effective: false, applied: null, supported: true, source: 'config', reason: null }
    ]
  };
  const rows = projectFeatureRows(snapshot, undefined);
  assert.strictEqual(rows[0].applied, null);
  assert.strictEqual(rows[0].requested, false);
}

console.log('all tests passed');
