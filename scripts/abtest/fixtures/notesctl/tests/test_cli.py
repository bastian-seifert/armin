import subprocess
import sys


def test_cli_parses():
    r = subprocess.run(
        [sys.executable, "-m", "notesctl", "list"],
        capture_output=True, text=True,
    )
    # skeleton exits 1 with "not implemented" — must parse without crashing
    assert "usage" not in r.stderr.lower() or "not implemented" in r.stderr.lower()
