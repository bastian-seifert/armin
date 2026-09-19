# notesctl

A minimal note-taking command line tool.

    python3 -m notesctl add "buy milk"
    python3 -m notesctl list

Status: persistent storage implemented — backend is a single JSON file,
storage location is configured via the NOTESCTL_PATH environment variable
(default ~/.notesctl when unset). Standard library only; no third-party
dependencies.
