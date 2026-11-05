import fcntl
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
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
        self.ports = []
        for _ in range(2):
            with socket.socket() as listener:
                listener.bind(('127.0.0.1', 0))
                self.ports.append(listener.getsockname()[1])
        adapter = self.root / 'adapter'
        adapter.mkdir()
        self.script = adapter / 'reference.sh'
        source = (ROOT / 'adapters/vmangos/reference.sh').read_text()
        for old, port in zip(('13724', '18085'), self.ports):
            source = source.replace(old, str(port))
        self._write_executable(self.script, source)
        self._write_executable(
            self.bin / 'docker',
            """#!/usr/bin/env python3
import os
from pathlib import Path
import sys
import time
with Path(os.environ['DOCKER_LOG']).open('a') as log:
    log.write(' '.join(sys.argv[1:]) + '\\n')
if 'up' in sys.argv and os.environ.get('WAIT_FOR_PORTS') == '1':
    Path(os.environ['UP_STARTED']).touch()
    deadline = time.monotonic() + 5
    while not Path(os.environ['PORTS_READY']).exists():
        if time.monotonic() > deadline:
            raise SystemExit(1)
        time.sleep(0.01)
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
    def _listen_for_reference(self):
        for port in self.ports:
            listener = socket.socket()
            self.addCleanup(listener.close)
            listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            listener.bind(('127.0.0.1', port))
            listener.listen()

    def _write_executable(self, path, source):
        path.write_text(source)
        path.chmod(0o700)

    def _environment(self):
        return dict(os.environ, HOME=str(self.home), WIRE_BIN=str(self.wire),
                    DOCKER_LOG=str(self.root / 'docker.log'),
                    PATH=f'{self.bin}:{Path(sys.executable).parent}:/usr/bin:/bin')

    def _command(self):
        return ['/bin/bash', str(self.script), 'record-checked',
                str(self.scenario), str(self.output)]

    def test_failed_shutdown_rejects_and_removes_recording(self):
        up_started = self.root / 'up-started'
        ports_ready = self.root / 'ports-ready'
        env = dict(self._environment(), WAIT_FOR_PORTS='1', UP_STARTED=str(up_started),
                   PORTS_READY=str(ports_ready))
        with subprocess.Popen(self._command(), env=env, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, text=True) as process:
            deadline = time.monotonic() + 5
            while not up_started.exists() and process.poll() is None:
                if time.monotonic() > deadline:
                    break
                time.sleep(0.01)
            self.assertTrue(up_started.exists())
            self._listen_for_reference()
            ports_ready.touch()
            _, stderr = process.communicate(timeout=10)

        self.assertNotEqual(process.returncode, 0, stderr)
        self.assertIn('stop failed; run reference.sh down', stderr)
        self.assertFalse(self.output.exists())

    def test_busy_ports_never_start_or_stop_another_reference_server(self):
        self._listen_for_reference()
        result = subprocess.run(self._command(), env=self._environment(),
                                capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 3, result.stderr)
        self.assertIn('ports are in use', result.stderr)
        commands = (self.root / 'docker.log').read_text().splitlines()
        self.assertTrue(commands)
        self.assertTrue(all(' ps ' in command for command in commands), commands)
        self.assertFalse(self.output.exists())

    def test_checked_recording_rejects_held_reference_lock(self):
        env = self._environment()
        with (self.reference / '.audit.lock').open('w') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            result = subprocess.run(
                ['/bin/bash', str(self.script), 'record-checked',
                 str(self.scenario), str(self.output)],
                env=env, capture_output=True, text=True,
            )

        self.assertEqual(result.returncode, 3, result.stderr)
        self.assertIn('another recording or audit holds the lock', result.stderr)
        self.assertFalse(self.output.exists())

    def test_spell_roll_check_records_once_and_cleans_all_characters(self):
        self.scenario.write_text("[spell_rolls]\nreference_characters = 16\n")
        for peer in range(2, 17):
            (self.reference / f'credentials/peer{peer}-password').write_text('peer\n')
        (self.script.parent / 'setup.py').write_text('import sys\n')
        self._write_executable(
            self.bin / 'docker',
            "#!/bin/sh\nprintf 'auth\\ndatabase\\nworld\\n'\n",
        )
        self._write_executable(
            self.wire,
            """#!/usr/bin/env python3
import os
from pathlib import Path
import sys
with Path(os.environ['WIRE_LOG']).open('a') as log:
    log.write(' '.join(sys.argv[1:]) + '\\n')
if sys.argv[1] == 'record':
    Path(sys.argv[4]).write_text('{}\\n')
""",
        )
        self._listen_for_reference()
        log = self.root / 'wire.log'
        result = subprocess.run(
            self._command(), env=dict(self._environment(), WIRE_LOG=str(log)),
            capture_output=True, text=True, timeout=10,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = log.read_text().splitlines()
        self.assertEqual(sum(call.startswith('record ') for call in calls), 1)
        self.assertFalse(any(call.startswith('compare ') for call in calls))
        resets = [call for call in calls if call.startswith('scenario char-delete ')]
        self.assertEqual(len(resets), 32)
        self.assertTrue(any('--account CONFORMANCE16 --character Rollpi' in call
                            for call in resets))
        self.assertTrue(self.output.exists())


if __name__ == '__main__':
    unittest.main()
