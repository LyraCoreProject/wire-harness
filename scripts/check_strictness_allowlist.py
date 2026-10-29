import json
import os
from pathlib import Path
import subprocess


ALLOWLIST = "strictness_allowlist.json"


def additions(current, base):
    if base is None:
        return []
    approved = {(entry["scenario"], entry["id"]) for entry in base["entries"]}
    return sorted(
        (entry["scenario"], entry["id"])
        for entry in current["entries"]
        if (entry["scenario"], entry["id"]) not in approved
    )


def main():
    base_ref = os.environ["BASE_REF"]
    base_commit = subprocess.check_output(
        ["git", "merge-base", "HEAD", f"origin/{base_ref}"], text=True
    ).strip()
    current = json.loads(Path(ALLOWLIST).read_text())
    baseline_file = f"{base_commit}:{ALLOWLIST}"
    base_path = subprocess.check_output(
        ["git", "ls-tree", "--name-only", base_commit, "--", ALLOWLIST], text=True
    ).strip()
    if not base_path:
        print("No base allowlist; this PR establishes the initial approved entries")
        return
    base = json.loads(subprocess.check_output(["git", "show", baseline_file], text=True))
    new_entries = additions(current, base)
    if new_entries:
        for scenario, mutation_id in new_entries:
            print(f"New strictness allowlist entry: {scenario}/{mutation_id}")
        raise SystemExit(
            "A maintainer must approve each addition by changing the base allowlist "
            "in its own reviewed PR."
        )


if __name__ == "__main__":
    main()
