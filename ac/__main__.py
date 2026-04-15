# __main__.py  -- supports both:
#   python -m ac <ac-style args>   (new)
#   python -m ac --input 0 ...     (legacy argparse)
import os
import shutil
import sys


def _resolve_rust_bin(name: str) -> str | None:
    """Look up a Rust binary by name: $PATH first, then the dev build tree."""
    found = shutil.which(name)
    if found:
        return found
    here = os.path.dirname(os.path.abspath(__file__))
    dev = os.path.join(here, "..", "ac-rs", "target", "debug", name)
    if os.path.isfile(dev) and os.access(dev, os.X_OK):
        return os.path.abspath(dev)
    return None


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "--serve":
        daemon = _resolve_rust_bin("ac-daemon")
        if daemon:
            os.execv(daemon, [daemon, "--local"])
        print("error: ac-daemon not found — build it with: cd ac-rs && cargo build -p ac-daemon",
              file=sys.stderr)
        sys.exit(1)

    # `ac ui ...` replaces the Python process with the Rust GPU UI. All trailing
    # args are passed through so `ac ui --synthetic --channels 4` behaves the
    # same as calling `ac-ui --synthetic --channels 4` directly.
    if len(sys.argv) > 1 and sys.argv[1] == "ui":
        ui = _resolve_rust_bin("ac-ui")
        if ui:
            os.execvp(ui, [ui, *sys.argv[2:]])
        print("error: ac-ui not found — build it with: cd ac-rs && cargo build -p ac-ui",
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
