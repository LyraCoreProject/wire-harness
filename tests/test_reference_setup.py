import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('reference_setup', Path(__file__).resolve().parents[1] / 'adapters/vmangos/setup.py')
setup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(setup)


class ReferenceSetup(unittest.TestCase):
    def setUp(self):
        scratch = Path.home() / '.cache/lyracore-scratch/conformance/harness/setup-tests'
        scratch.mkdir(parents=True, exist_ok=True)
        directory = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        root = patch.object(setup, 'ROOT', self.root)
        root.start()
        self.addCleanup(root.stop)

    def test_content_addressed_cache_survives_disappearing_release_assets(self):
        pins = {}
        for kind in ['binary', 'database']:
            payload = (kind + ' archive').encode()
            checksum = hashlib.sha256(payload).hexdigest()
            pins.update({kind + '_asset': 'deleted-' + kind, kind + '_sha256': checksum, kind + '_asset_id': 7})
            cached = self.root / 'cache/sha256' / checksum
            cached.parent.mkdir(parents=True, exist_ok=True)
            cached.write_bytes(payload)
        with patch.object(setup, 'PINS', pins), patch.object(setup.subprocess, 'run', side_effect=AssertionError('network must not be used')):
            for kind in ['binary', 'database']:
                self.assertEqual(setup.asset(kind).read_bytes(), (kind + ' archive').encode())

    def test_corrupt_cache_fails_without_a_network_fallback(self):
        checksum = hashlib.sha256(b'archive').hexdigest()
        cached = self.root / 'cache/sha256' / checksum
        cached.parent.mkdir(parents=True)
        cached.write_bytes(b'corrupted')
        with patch.object(setup, 'PINS', {'binary_asset': 'missing', 'binary_sha256': checksum}), patch.object(setup.subprocess, 'run', side_effect=AssertionError('network must not be used')):
            with self.assertRaisesRegex(RuntimeError, 'cached archive checksum mismatch'):
                setup.asset('binary')

    def test_named_legacy_archive_moves_into_verified_content_cache(self):
        payload = b'archive'
        checksum = hashlib.sha256(payload).hexdigest()
        legacy = self.root / 'downloads/test-archive'
        legacy.parent.mkdir()
        legacy.write_bytes(payload)
        with patch.object(setup, 'PINS', {'binary_asset': legacy.name, 'binary_sha256': checksum}), patch.object(setup.subprocess, 'run', side_effect=AssertionError('network must not be used')):
            cached = setup.asset('binary')
            self.assertEqual(cached, self.root / 'cache/sha256' / checksum)
            self.assertEqual(cached.read_bytes(), payload)

    def test_configuration_check_refuses_enabled_gm_cheats_and_duplicate_settings(self):
        config = self.root / 'config/mangosd.conf'
        config.parent.mkdir()
        valid = ''.join(f'{key} = {value}\n' for key, value in setup.GM_CONFIG.items())
        config.write_text(valid)
        setup.verify_configuration()
        for invalid in [valid.replace('GM.CheatGod = 0', 'GM.CheatGod = 1'), valid.replace('GM.LoginState = 0', 'GM.LoginState = 2'), valid + 'GM.CheatGod = 0\n']:
            config.write_text(invalid)
            with self.assertRaises(RuntimeError):
                setup.verify_configuration()

    def test_gameplay_account_requires_zero_global_and_realm_security(self):
        for account, grants, valid in [('0', '0', True), ('3', '0', False), ('0', '0\n3', False), ('', '', False)]:
            replies = iter([account, grants])
            with patch.object(setup.subprocess, 'run', side_effect=lambda *a, **kw: setup.subprocess.CompletedProcess(a, 0, next(replies))):
                if valid:
                    setup.verify_account('CONFORMANCE')
                else:
                    with self.assertRaisesRegex(RuntimeError, 'gmlevel 0'):
                        setup.verify_account('CONFORMANCE')


if __name__ == '__main__':
    unittest.main()
