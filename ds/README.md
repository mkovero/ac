# ds — diagnostic sessions

Companion CLI for `ac` (thd_tool). Reads ac config and session directories to provide AI-powered analysis, notes, file management, and session diffing. Never writes to ac's data or requires ZMQ.

## Requirements

```bash
export ANTHROPIC_API_KEY=sk-ant-...
```

Required for `analyze`, `ask`, `diff`, and `fetch` commands.

## Install

Installed alongside thd_tool:

```bash
pip install -e .
```

Entry point: `ds` (or `python -m ds`).

## Commands

| Command | Description |
|---------|-------------|
| `ds status` | Show active session, files, and note count |
| `ds ls` | List ac and ds files with sizes and dates |
| `ds note "<text>"` | Add a timestamped note to the session |
| `ds notes` | List all notes in chronological order |
| `ds analyze` | AI analysis: fault candidates, anomalies, next steps |
| `ds ask "<question>"` | Ask a question with full session context |
| `ds diff <before> <after>` | AI comparison of two sessions' measurements |
| `ds log [--last N]` | Show recent AI interactions (default: last 5) |
| `ds add <path>` | Copy a file into the session's ds/files/ |
| `ds rm <filename>` | Remove a file from ds/files/ |
| `ds fetch [query]` | Web search for service manuals/schematics |

## Configuration

ds reads `~/.config/thd_tool/config.json`, which is created and managed by `ac`. See [`../config.example.jsonc`](../config.example.jsonc) for the full schema with comments.

Quick start from scratch:

```bash
ac setup output 11 input 0   # creates config.json
ac new my-amp                 # sets active session
ds status                     # ds picks it up automatically
```

## Relationship to ac

- Reads `~/.config/thd_tool/config.json` for the active session name
- Reads session directories under `~/.local/share/thd_tool/sessions/`
- Stores its own data in a `ds/` subdirectory within each session
- No ZMQ connection, no audio I/O, no writes to ac data
