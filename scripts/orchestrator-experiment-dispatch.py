#!/usr/bin/env python3
"""Enter the immutable experiment bundle before locating its process broker."""
import os
from pathlib import Path
import sys


def main(arguments):
    try:
        executable = (Path(__file__).resolve().parent / 'orchestrator-experiment').resolve(strict=True)
        os.execve(str(executable), [str(executable)] + arguments, dict(os.environ))
    except OSError as error:
        print('orchestrator-experiment: cannot start bundled runner: ' + str(error), file=sys.stderr)
        return 126


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
