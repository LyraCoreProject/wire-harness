import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from check_strictness_allowlist import additions


class StrictnessAllowlistTest(unittest.TestCase):
    def test_new_scenario_and_existing_scenario_additions(self):
        base = {"entries": [{"scenario": "existing", "id": "old"}]}
        current = {
            "entries": [
                {"scenario": "existing", "id": "old"},
                {"scenario": "existing", "id": "new"},
                {"scenario": "new_scenario", "id": "survivor"},
            ]
        }
        self.assertEqual(
            additions(current, base),
            [("existing", "new"), ("new_scenario", "survivor")],
        )

    def test_removal_and_initial_allowlist(self):
        base = {"entries": [{"scenario": "existing", "id": "old"}]}
        current = {"entries": []}
        self.assertEqual(additions(current, base), [])
        self.assertEqual(additions(base, None), [])


if __name__ == "__main__":
    unittest.main()
