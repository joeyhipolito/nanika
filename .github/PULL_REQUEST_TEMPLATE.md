## Test IDs

List every test ID (function name or `t.Run` label) added or modified in this PR. All listed IDs must exist in the code and CI must have run them.

```
# e.g.
# TestFoo/happy_path
# TestBar
```

## Test Discipline

```yaml
phase: ""
test_ids: []
rfc_sections: []
```

- [ ] I wrote tests first (RED) before implementation
- [ ] Test commit precedes impl commit in git log for touched paths
- [ ] All test IDs listed above exist in the code and CI ran them
- [ ] No orphan TODO/FIXME without a tracker reference

See RFC §10.1 for test-first discipline requirements.
