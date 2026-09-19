"""notesctl command line interface."""
import argparse
import sys

from . import storage


def main(argv=None):
    parser = argparse.ArgumentParser(prog="notesctl",
                                     description="Store and list short notes.")
    sub = parser.add_subparsers(dest="command", required=True)

    p_add = sub.add_parser("add", help="store a new note")
    p_add.add_argument("text", help="note text")

    sub.add_parser("list", help="list all stored notes")

    args = parser.parse_args(argv)

    if args.command == "add":
        note = storage.add_note(args.text)
        print(f"added note {note['id']}")
        return 0
    if args.command == "list":
        for note in storage.list_notes():
            print(f"{note['id']}: {note['text']}")
        return 0
    return 1


if __name__ == "__main__":
    sys.exit(main())
