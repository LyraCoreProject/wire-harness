import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]


class ReferenceWrapperTests(unittest.TestCase):
    def setUp(self):
        scratch = Path.home() / '.cache/lyracore-scratch/conformance/harness/wrapper-tests'
        scratch.mkdir(parents=True, exist_ok=True)
        self.directory = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.home = self.root / 'home'
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        self.reference = self.home / '.cache/lyracore-scratch/reference-server'
        (self.reference / 'credentials').mkdir(parents=True)
        (self.reference / 'credentials/player-password').write_text('player\n')
        (self.reference / 'credentials/gm-password').write_text('gm\n')
        (self.reference / 'provenance.json').write_text('{}\n')
        self.scenario = self.root / 'scenario.toml'
        self.scenario.write_text("[[fixtures]]\nverb = 'character'\nname = 'Fixture'\n")
        self.output = self.root / 'trace.json'
        self._write_executable(
            self.bin / 'docker',
            """#!/usr/bin/env python3
import sys
if 'ps' in sys.argv:
    raise SystemExit(0)
if 'down' in sys.argv:
    raise SystemExit(1)
""",
        )
        self.wire = self.bin / 'wire'
        self._write_executable(
            self.wire,
            """#!/usr/bin/env python3
from pathlib import Path
import sys
if sys.argv[1] == 'record':
    Path(sys.argv[4]).write_text('{}\\n')
elif sys.argv[1] == 'compare':
    print('{}')
""",
        )
        self.listeners = []
        for port in (13724, 18085):
            listener = socket.socket()
            listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            listener.bind(('127.0.0.1', port))
            listener.listen()
            self.listeners.append(listener)
            self.addCleanup(listener.close)

    def _write_executable(self, path, source):
        path.write_text(source)
        path.chmod(0o700)

    def test_failed_shutdown_rejects_and_removes_recording(self):
        env = dict(os.environ, HOME=str(self.home), WIRE_BIN=str(self.wire),
                   PATH=f'{self.bin}:{Path(sys.executable).parent}:/usr/bin:/bin')
        result = subprocess.run(
            ['/bin/bash', str(ROOT / 'adapters/vmangos/reference.sh'), 'record-checked',
             str(self.scenario), str(self.output)],
            env=env, capture_output=True, text=True,
        )

        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertIn('stop failed; run reference.sh down', result.stderr)
        self.assertFalse(self.output.exists())


if __name__ == '__main__':
    unittest.main()
