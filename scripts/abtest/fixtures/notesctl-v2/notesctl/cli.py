"""notesctl command line interface (storage not implemented yet)."""
import argparse
import sys


def main(argv=None):
    parser = argparse.ArgumentParser(prog="notesctl",
                                     description="Store and list short notes.")
    sub = parser.add_subparsers(dest="command", required=True)

    p_add = sub.add_parser("add", help="store a new note")
    p_add.add_argument("text", help="note text")

    sub.add_parser("list", help="list all stored notes")

    args = parser.parse_args(argv)

    if args.command == "add":
        print("not implemented: storage backend missing", file=sys.stderr)
        return 1
    if args.command == "list":
        print("not implemented: storage backend missing", file=sys.stderr)
        return 1
    return 1


if __name__ == "__main__":
    sys.exit(main())
