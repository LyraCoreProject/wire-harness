import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]


class AdapterTests(unittest.TestCase):
    def setUp(self):
        scratch = Path.home() / '.cache/lyracore-scratch/conformance/adapter-tests'
        scratch.mkdir(parents=True, exist_ok=True)
        self.directory = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(self.directory.cleanup)
        self.binary = Path(self.directory.name) / 'client'
        self.binary.write_text('#!/usr/bin/env python3\nimport json, sys\nprint(json.dumps([sys.argv[1:], sys.stdin.read()]))\n')
        self.binary.chmod(0o700)
        self.env = {k: v for k, v in os.environ.items() if not k.startswith('WIRE_')}
        self.env['WIRE_BIN'] = str(self.binary)

    def run_wire(self, *args, password='fixture-input\n'):
        return subprocess.run(['bash', str(ROOT / 'adapters/lyracore/wire.sh'), *args],
                              env=self.env, input=password, capture_output=True, text=True)

    def test_smoke_keeps_password_on_stdin_and_passes_endpoints(self):
        self.env.update(WIRE_LOGON_PORT='13724', WIRE_WORLD_PORT='18085')
        result = self.run_wire('ACCOUNT', 'Character')
        self.assertEqual(result.returncode, 0, result.stderr)
        argv, password = json.loads(result.stdout)
        self.assertEqual(argv, ['smoke', '--account', 'ACCOUNT', '--character', 'Character',
                               '--password-stdin', '--host', '127.0.0.1', '--logon-port',
                               '13724', '--world-port', '18085', '--class', 'warrior', '--race', 'human'])
        self.assertEqual(password, 'fixture-input\n')

    def test_explicit_account_password_wins_over_shared_fixture_password(self):
        self.env.update(WIRE_PASSWORD_ACCOUNT='account-input', WIRE_FIXTURE_PASSWORD='shared-input')
        result = self.run_wire('account', 'Character')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)[1], 'account-input\n')

    def test_no_implicit_password(self):
        result = self.run_wire('ACCOUNT', 'Character', password='')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)[1], '')

    def test_native_checkout_resolves_without_legacy_directory(self):
        checkout = Path(self.directory.name) / 'core'
        (checkout / 'native-host').mkdir(parents=True)
        (checkout / 'module').mkdir()
        (checkout / 'Cargo.toml').touch()
        env = dict(self.env, LYRACORE_DIR=str(checkout))
        result = subprocess.run(['bash', '-c', 'source "$1"; resolve_lyracore', 'bash',
                                 str(ROOT / 'adapters/lyracore/adapter-env.sh')], env=env, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_invalid_account_fails_before_client(self):
        self.assertEqual(self.run_wire('invalid-name', 'Character').returncode, 2)
