#!/usr/bin/python3
import pathlib
import sys
assert sys.argv[1:] == ["literal; $(not-a-command)"]
sys.exit(0 if pathlib.Path("note.txt").read_bytes() == b"after\n" else 1)
