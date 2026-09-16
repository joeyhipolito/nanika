#!/usr/bin/env python3
"""Build a matching Rust pilot/broker and Go compatibility bundle, then activate it."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile
import uuid

ROOT = Path(__file__).resolve().parent.parent
NAMES = ('orchestrator-first-use-pilot', 'orchestrator-process-broker',
         'orchestrator-output', 'orchestrator-usage-replay')
LINKS = {'orchestrator': 'orchestrator', 'orchestrator-go': 'orchestrator-go',
         'orchestrator-output': 'orchestrator-output',
         'orchestrator-usage-replay': 'orchestrator-usage-replay'}


def run(command, **kwargs):
    subprocess.run(command, check=True, **kwargs)


def install(prefix, profile):
    if platform.system() not in ('Darwin', 'Linux'):
        raise ValueError('Rust process supervision currently supports macOS and Linux')
    prefix = prefix.expanduser().absolute()
    if prefix.is_symlink():
        raise ValueError(f'expected a real prefix directory: {prefix}')
    prefix = prefix.resolve()
    bindir = prefix / 'bin'
    bundles = prefix / 'libexec' / 'nanika-orchestrator'
    # Refuse non-directory parents and unmanaged outputs before building anything.
    for directory in (prefix, bindir, prefix / 'libexec', bundles):
        if directory.is_symlink() or (directory.exists() and not directory.is_dir()):
            raise ValueError(f'expected a real directory: {directory}')
    for name in LINKS:
        link = bindir / name
        if link.exists() or link.is_symlink():
            if not link.is_symlink() or not link.resolve().is_relative_to(bundles):
                raise ValueError(f'preserving unmanaged entry {link}; use another --prefix')
    bindir.mkdir(parents=True, exist_ok=True)
    bundles.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.build-', dir=bundles) as temporary:
        work = Path(temporary)
        manifest = ROOT / 'skills/orchestrator-rs/Cargo.toml'
        target = Path(os.environ.get('CARGO_TARGET_DIR', str(work / 'target'))).absolute()
        run(['cargo', 'build', '--manifest-path', str(manifest), '--locked',
             '--target-dir', str(target), '--profile', profile,
             '-p', 'orchestrator-first-use-pilot', '--bins',
             '-p', 'orchestrator-process', '--bin', 'orchestrator-process-broker'])
        stage = work / 'bundle'
        stage.mkdir()
        for name in NAMES:
            shutil.copy2(target / ('debug' if profile == 'dev' else profile) / name, stage / name)
        environment = dict(os.environ, GOWORK='off')
        run(['go', 'build', '-o', str(stage / 'orchestrator-go'), '.'],
            cwd=ROOT / 'skills/orchestrator', env=environment)
        shutil.copy2(ROOT / 'scripts/orchestrator-dispatch.py', stage / 'orchestrator')
        hashes = {f.name: hashlib.sha256(f.read_bytes()).hexdigest() for f in sorted(stage.iterdir())}
        revision = subprocess.check_output(['git', '-C', str(ROOT), 'rev-parse', 'HEAD'], text=True).strip()
        dirty = bool(subprocess.check_output(['git', '-C', str(ROOT), 'status', '--porcelain']))
        identity = {'sha256': hashes, 'revision': revision, 'source_dirty': dirty}
        bundle_id = hashlib.sha256(json.dumps(identity, sort_keys=True).encode()).hexdigest()[:20]
        destination = bundles / bundle_id
        (stage / 'manifest.json').write_text(json.dumps({
            'schema': 1, 'source_revision': revision, 'profile': profile,
            'platform': platform.system(), 'architecture': platform.machine(),
            'sha256': hashes, 'bundle_id': bundle_id,
            'source_dirty': dirty,
        }, indent=2) + '\n')
        if destination.exists():
            for name, digest in hashes.items():
                if hashlib.sha256((destination / name).read_bytes()).hexdigest() != digest:
                    raise ValueError(f'existing bundle differs: {destination}')
        else:
            os.rename(stage, destination)
        previous = str((bindir / 'orchestrator').resolve()) if (bindir / 'orchestrator').exists() else None
        # Activate the dispatcher last. It resolves its own bundle before selecting an engine.
        for name in sorted(LINKS, key=lambda item: item == 'orchestrator'):
            temporary_link = bindir / ('.' + name + '-' + uuid.uuid4().hex)
            try:
                temporary_link.symlink_to(destination / LINKS[name])
                os.replace(temporary_link, bindir / name)
            finally:
                temporary_link.unlink(missing_ok=True)
        print(json.dumps({'installed': str(bindir / 'orchestrator'), 'bundle': str(destination),
                          'previous_dispatcher': previous, 'default': 'rust-first-with-go-compatibility'}, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prefix', type=Path, default=Path.home() / '.local')
    parser.add_argument('--profile', choices=('dev', 'release'), default='release')
    args = parser.parse_args()
    try:
        install(args.prefix, args.profile)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f'install-rust-orchestrator: {error}\n')


if __name__ == '__main__':
    main()
