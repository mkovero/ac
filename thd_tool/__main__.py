# __main__.py  -- supports both:
#   python -m thd_tool <ac-style args>   (new)
#   python -m thd_tool --input 0 ...     (legacy argparse)
import sys

def main():
    if len(sys.argv) > 1 and sys.argv[1] == "--serve":
        from .server.server import run_server
        run_server()
        sys.exit(0)
    # If first arg looks like a legacy --flag, use old cli
    if len(sys.argv) > 1 and sys.argv[1].startswith("--"):
        from .cli import main as legacy_main
        legacy_main()
    else:
        from .client.ac import main as ac_main
        ac_main()

if __name__ == "__main__":
    main()
