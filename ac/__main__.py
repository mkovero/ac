# __main__.py  -- supports both:
#   python -m ac <ac-style args>   (new)
#   python -m ac --input 0 ...     (legacy argparse)
import sys

def main():
    if len(sys.argv) > 1 and sys.argv[1] == "--serve":
        import os
        import shutil
        daemon = shutil.which("ac-daemon")
        if not daemon:
            _here = os.path.dirname(os.path.abspath(__file__))
            _dev  = os.path.join(_here, "..", "ac-rs", "target", "debug", "ac-daemon")
            if os.path.isfile(_dev) and os.access(_dev, os.X_OK):
                daemon = os.path.abspath(_dev)
        if daemon:
            os.execv(daemon, [daemon, "--local"])
        print("error: ac-daemon not found — build it with: cd ac-rs && cargo build -p ac-daemon",
              file=sys.stderr)
        sys.exit(1)
    # If first arg looks like a legacy --flag, use old cli
    if len(sys.argv) > 1 and sys.argv[1].startswith("--"):
        from .cli import main as legacy_main
        legacy_main()
    else:
        from .client.ac import main as ac_main
        ac_main()

if __name__ == "__main__":
    main()
