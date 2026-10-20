# Repository Guidelines

## Project Structure & Module Organization

This repository currently centers on a Python MCP bridge for the PSR-E463 diagnostic shell. `psr_shell_mcp.py` contains the server, MIDI transport, and shell tools; `pyproject.toml` defines package metadata and dependencies. Shell protocol and setup documentation live in `README_PSR_SHELL_MCP.md` and `PSR_Shell_usage.md`. Reverse-engineering notes are in `NOTES_EMULATOR_REQUIREMENTS.md`, `XF_CHORD_LCD_NOTES.md`, and `PSR_SHELL_ADDRESS_SAFETY.md`. `PSR-Shell.html`, `ref.vectorcraft`, and `ref.png` are reference assets. There is no dedicated `src/` or test directory yet; keep additions grouped by purpose and add tests under `tests/` when a suite is introduced.

## Build, Test, and Development Commands

Use Python 3.10 or newer. Create an isolated environment and install the package in editable mode:

```sh
python -m venv .venv
source .venv/bin/activate
python -m pip install -e .
```

Run the MCP stdio server with `psr-shell-mcp` (or `python psr_shell_mcp.py` if invoking the module directly). MIDI operations require visible keyboard MIDI ports. No automated test or lint command is currently configured; do not assume hardware is available in local checks.

## Coding Style & Naming Conventions

Follow the existing Python style: four-space indentation, `snake_case` functions and variables, `UPPER_SNAKE_CASE` constants, type hints for public helpers, and concise docstrings for MCP tools. Keep MIDI encoding/decoding separate from tool orchestration where practical. Use the standard library unless a dependency is justified and recorded in `pyproject.toml`.

## Testing Guidelines

There is no established test framework or coverage requirement. For changes to protocol helpers, add focused unit tests under `tests/` using `pytest` if introducing that dependency, and keep tests independent of attached hardware. Exercise the real MIDI connection only as an explicit manual check.

## Commit & Pull Request Guidelines

The Git history is empty, so no repository-specific commit convention is established. Use a short imperative subject (for example, `Document LCD segment mapping`). Pull requests should summarize behavior changes, list validation performed, link related work when applicable, and include screenshots only for visual artifacts.

## Hardware and Safety

The MCP server forwards shell input without a command allowlist. Treat `shell_send` and `shell_command` as unrestricted diagnostic access. Follow `PSR_SHELL_ADDRESS_SAFETY.md`: keep hardware exploration read-only unless a memory region and recovery procedure have been independently verified. Never infer safe writes from Ghidra image ranges or successful reads.
