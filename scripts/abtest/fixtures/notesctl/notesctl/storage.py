# Backend: json
# Config: env
"""Persistent note storage for notesctl.

Binding decisions for this project:

- Backend: a single JSON file. All notes live in one file named
  ``notes.json``. Writes are atomic (write to a temporary file in the
  same directory, then ``os.replace``) so a crash never leaves a
  half-written store.

- Config: the environment variable ``NOTESCTL_PATH``. It names the
  directory in which ``notes.json`` is stored; the directory is created
  on first use. If the variable is unset, ``~/.notesctl`` is used as
  the fallback default location. No config file is read or written.

Standard library only.
"""

import json
import os
import tempfile
from datetime import datetime, timezone

STORE_FILENAME = "notes.json"
ENV_VAR = "NOTESCTL_PATH"


def storage_dir():
    """Return the directory that holds the store (creating it if needed)."""
    base = os.environ.get(ENV_VAR, "").strip()
    if not base:
        base = os.path.join(os.path.expanduser("~"), ".notesctl")
    os.makedirs(base, exist_ok=True)
    return base


def _store_path():
    return os.path.join(storage_dir(), STORE_FILENAME)


def load_notes():
    """Return all stored notes as a list (empty list if no store yet)."""
    path = _store_path()
    if not os.path.exists(path):
        return []
    with open(path, "r", encoding="utf-8") as f:
        data = json.load(f)
    if not isinstance(data, list):
        raise ValueError(f"corrupt store: {path} does not contain a JSON list")
    return data


def save_notes(notes):
    """Atomically persist the full list of notes."""
    path = _store_path()
    directory = os.path.dirname(path)
    fd, tmp = tempfile.mkstemp(prefix=".notes-", suffix=".tmp", dir=directory)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            json.dump(notes, f, ensure_ascii=False, indent=2)
            f.write("\n")
        os.replace(tmp, path)
    except BaseException:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise


def add_note(text):
    """Append a note and persist the store. Returns the stored note dict."""
    notes = load_notes()
    note = {
        "id": max((n.get("id", 0) for n in notes), default=0) + 1,
        "text": text,
        "created": datetime.now(timezone.utc).isoformat(timespec="seconds"),
    }
    notes.append(note)
    save_notes(notes)
    return note


def list_notes():
    """Return all notes in insertion order."""
    return load_notes()
