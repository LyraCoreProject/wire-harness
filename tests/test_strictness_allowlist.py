from contextlib import redirect_stdout
from io import StringIO
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from check_strictness_allowlist import additions, main


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

    def test_ci_rejects_new_entry_at_merge_base(self):
        current = '{"entries":[{"scenario":"new_scenario","id":"survivor"}]}'
        base = '{"entries":[]}'
        output = StringIO()
        with (
            redirect_stdout(output),
            patch.dict("os.environ", {"BASE_REF": "main"}),
            patch("check_strictness_allowlist.Path.read_text", return_value=current),
            patch(
                "check_strictness_allowlist.subprocess.check_output",
                side_effect=["base-commit\n", "strictness_allowlist.json\n", base],
            ) as git_output,
        ):
            with self.assertRaisesRegex(
                SystemExit,
                "A maintainer must approve each addition by changing the base allowlist",
            ):
                main()
        self.assertIn("new_scenario/survivor", output.getvalue())
        self.assertEqual(
            git_output.call_args_list[0].args[0],
            ["git", "merge-base", "HEAD", "origin/main"],
        )
        self.assertEqual(
            git_output.call_args_list[1].args[0],
            ["git", "ls-tree", "--name-only", "base-commit", "--", "strictness_allowlist.json"],
        )
        self.assertEqual(
            git_output.call_args_list[2].args[0],
            ["git", "show", "base-commit:strictness_allowlist.json"],
        )

    def test_ci_accepts_initial_allowlist(self):
        with (
            patch.dict("os.environ", {"BASE_REF": "main"}),
            patch(
                "check_strictness_allowlist.Path.read_text",
                return_value='{"entries":[{"scenario":"initial","id":"survivor"}]}',
            ),
            patch(
                "check_strictness_allowlist.subprocess.check_output",
                side_effect=["base-commit\n", ""],
            ) as git_output,
        ):
            main()
        self.assertEqual(git_output.call_count, 2)


if __name__ == "__main__":
    unittest.main()
