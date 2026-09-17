#!/usr/bin/env python3
"""Offline checks for command routing and a previously installed source bundle."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('dispatcher', ROOT / 'scripts/orchestrator-dispatch.py')
dispatcher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dispatcher)


class DispatchTests(unittest.TestCase):
    def test_native_and_legacy_selection(self):
        for args in [['code'], ['review'], ['resume'], ['observe'], ['view'], ['features'],
                     ['run', '--repo=x'], ['status', '--output-dir', 'x'],
                     ['cancel', '--output-dir=x']]:
            self.assertEqual(dispatcher.select(args), ('rust', args))
        for args in [['run', 'task'], ['status'], ['metrics'],
                     ['run', 'task', '--', '--repo=x']]:
            self.assertEqual(dispatcher.select(args), ('go', args))
        self.assertEqual(dispatcher.select(['--engine', 'go', 'review']), ('go', ['review']))
        with self.assertRaises(ValueError):
            dispatcher.select(['--engine', 'invalid'])

    def test_failed_rust_never_executes_go(self):
        with tempfile.TemporaryDirectory(prefix='dispatcher space ') as temporary:
            root = Path(temporary)
            wrapper = root / 'orchestrator'
            wrapper.write_bytes((ROOT / 'scripts/orchestrator-dispatch.py').read_bytes())
            wrapper.chmod(0o755)
            rust = root / 'orchestrator-first-use-pilot'
            rust.write_text('#!/bin/sh\ntest "$NANIKA_RUST_FIRST_USE_PILOT" = 1 || exit 99\nexit 23\n')
            rust.chmod(0o755)
            go = root / 'orchestrator-go'
            go.write_text('#!/bin/sh\nprintf GO-RAN\n')
            go.chmod(0o755)
            result = subprocess.run([str(wrapper), 'code'], capture_output=True, text=True)
            self.assertEqual(result.returncode, 23)
            self.assertNotIn('GO-RAN', result.stdout)
            rust.unlink()
            result = subprocess.run([str(wrapper), 'code'], capture_output=True, text=True)
            self.assertEqual(result.returncode, 126)
            self.assertNotIn('GO-RAN', result.stdout)


def installed(prefix):
    executable = prefix / 'bin/orchestrator'
    info = json.loads(subprocess.check_output([str(executable), '--engine-info'], text=True))
    assert info['default'] == 'rust-first-with-go-compatibility'
    assert info['fallback_after_execution'] is False
    rust = Path(info['rust'])
    assert rust.parent == Path(info['go']).parent
    assert (rust.parent / 'orchestrator-process-broker').is_file()
    for args in [['--help'], ['--engine', 'rust', '--help'], ['--engine', 'go', '--help']]:
        subprocess.run([str(executable), *args], check=True, capture_output=True)
    with tempfile.TemporaryDirectory(prefix='smoke-', dir=prefix) as temporary:
        progress = Path(temporary) / 'empty-progress.jsonl'
        progress.write_text('')
        subprocess.run([str(executable), 'observe', '--progress-log', str(progress)], check=True, capture_output=True)
        log = Path(temporary) / 'output.log'
        result = subprocess.run([str(prefix / 'bin/orchestrator-output'), '--portal', 'on',
                                 '--log', str(log), '--', '/bin/sh', '-c', 'printf fixture'],
                                check=True, capture_output=True)
        assert log.read_text() == 'fixture'
        assert json.loads(result.stdout)
    print('Installed paired engines, offline observation, and output helper passed.')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prefix', type=Path)
    args = parser.parse_args()
    result = unittest.TextTestRunner().run(unittest.defaultTestLoader.loadTestsFromTestCase(DispatchTests))
    if not result.wasSuccessful():
        raise SystemExit(1)
    if args.prefix:
        installed(args.prefix)
