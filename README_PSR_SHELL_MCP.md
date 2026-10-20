# PSR Shell MCP server

This server connects to the Yamaha MIDI port used by `PSR-Shell.html` and
exposes the diagnostic terminal as MCP tools. It follows the HTML's SysEx
protocol: the seven-byte Yamaha header `43 73 01 52 19 00 00`, followed by two
4-bit nibbles for each ASCII byte. The server does not interpret or validate
diagnostic-shell commands; commands are sent to the keyboard as typed.

## Install

From this directory, create and activate a virtual environment, then install
the project:

```sh
python -m venv .venv
source .venv/bin/activate
python -m pip install -e .
```

The MIDI backend must be able to see the keyboard's MIDI input and output ports.
On Linux, that generally means the host must have access to the relevant ALSA
or JACK MIDI devices.

## Run

```sh
psr-shell-mcp
```

The server uses MCP stdio transport. Configure the MCP host to launch the
command above with this project directory as its working directory and the
same Python environment in which the project was installed.

## Tools

- `midi_ports`: list visible MIDI input/output ports.
- `connect`: open a matching input and output port (default match: `Digital Keyboard`).
- `disconnect`: close the MIDI ports.
- `shell_send`: send ASCII text, including terminal control characters.
- `shell_read`: collect incoming terminal text for a timeout.
- `shell_command`: send one command followed by Enter and collect the response.

The diagnostic shell has the access granted by the keyboard itself. Treat
`shell_send` and `shell_command` as unrestricted access to that shell; this
bridge intentionally does not impose a command allowlist.