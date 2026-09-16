#!/usr/bin/env python3
"""Rust-first entry point; select legacy compatibility before any execution."""
import json
import os
from pathlib import Path
import sys

BIN = Path(__file__).resolve().parent
RUST = BIN / 'orchestrator-first-use-pilot'
GO = BIN / 'orchestrator-go'
HELP = """orchestrator: Rust-first entry point with Go compatibility

Rust commands:
  code / review                 Native Rust argument syntax
  run --repo ... --output-dir ... -- <verifier command>
  resume --output-dir <saved Rust run>
  status --output-dir <saved Rust run>
  cancel --output-dir <saved Rust run> --mission <exact-id>
  observe --progress-log <file> [--follow] [--format text|json]
  view --progress-log <file> [--follow]  Interactive mission inspector

Existing plain-text missions, global status, metrics, audit, daemon and
other legacy commands use Go. A notice on stderr identifies that selection.
Rust failures are returned directly; they never trigger a retry through Go.

  orchestrator --engine rust --help    Full native Rust usage
  orchestrator --engine go --help      Full legacy Go usage
  orchestrator --engine-info          Show installed engines

The full rewrite remains unfinished; native cancellation and observation are installed.
"""


def select(arguments):
    if arguments[:1] == ['--engine']:
        if len(arguments) < 2 or arguments[1] not in ('rust', 'go'):
            raise ValueError('--engine requires rust or go')
        return arguments[1], arguments[2:]
    if not arguments or arguments in (['--help'], ['-h'], ['help']):
        return 'help', []
    if arguments == ['--engine-info']:
        return 'info', []
    command = arguments[0]
    options = arguments[1:]
    if '--' in options:
        options = options[:options.index('--')]
    names = {option.split('=', 1)[0] for option in options if option.startswith('--')}
    if command in ('code', 'review', 'resume', 'observe', 'view'):
        return 'rust', arguments
    if command in ('run', 'status', 'cancel') and names.intersection({
        '--repo', '--output-dir', '--durable', '--task-file', '--mission-file', '--prompt-file'
    }):
        return 'rust', arguments
    return 'go', arguments


def main(arguments):
    try:
        engine, forwarded = select(arguments)
    except ValueError as error:
        print('orchestrator: ' + str(error), file=sys.stderr)
        return 2
    if engine == 'help':
        print(HELP, end='')
        return 0
    if engine == 'info':
        print(json.dumps({'default': 'rust-first-with-go-compatibility',
                          'rust': str(RUST.resolve()), 'go': str(GO.resolve()),
                          'fallback_after_execution': False}, indent=2))
        return 0
    executable = RUST if engine == 'rust' else GO
    environment = dict(os.environ)
    if engine == 'rust':
        environment['NANIKA_RUST_FIRST_USE_PILOT'] = '1'
    else:
        print('orchestrator: using Go compatibility for this command', file=sys.stderr)
    try:
        # macOS current_exe preserves a symlink path; the Rust broker is a sibling
        # of the immutable binary, so select that installation before exec.
        if engine == 'rust':
            executable = executable.resolve(strict=True)
        os.execve(str(executable), [str(executable)] + forwarded, environment)
    except OSError as error:
        print('orchestrator: cannot start ' + engine + ': ' + str(error), file=sys.stderr)
        return 126


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
