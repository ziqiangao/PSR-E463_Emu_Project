"""MCP bridge for the PSR-Shell.html Yamaha MIDI SysEx terminal.

The shell transports ASCII over System Exclusive messages. This server does not
interpret diagnostic-shell commands; it only sends and receives terminal text.
Read PSR_Shell_usage.md to see how to interface this shell.
"""

from __future__ import annotations

import queue
import threading
import time
from typing import Any

import mido
from mcp.server.fastmcp import FastMCP


MIDI_PORT_NAME = "Digital Keyboard"
SYSEX_HEADER = (0x43, 0x73, 0x01, 0x52, 0x19, 0x00, 0x00)
MAX_TEXT_LENGTH = 2048

mcp = FastMCP("PSR-E463 Diagnostic Shell")
_lock = threading.RLock()
_input: Any | None = None
_output: Any | None = None
_received: queue.Queue[str] = queue.Queue(maxsize=1000)
_rx_overflow = 0


from pathlib import Path

@mcp.resource("psr-shell://usage")
def shell_usage() -> str:
    """Usage guide for the PSR diagnostic shell."""
    path = Path(__file__).with_name("PSR_Shell_usage.md")
    return path.read_text(encoding="utf-8")


def _find_port(names: list[str], requested: str) -> str:
    matches = [name for name in names if requested.casefold() in name.casefold()]
    if not matches:
        raise RuntimeError(
            f"No MIDI port contains {requested!r}. Available ports: {names or '(none)'}"
        )
    if len(matches) > 1:
        exact = [name for name in matches if name.casefold() == requested.casefold()]
        if len(exact) == 1:
            return exact[0]
        raise RuntimeError(f"Multiple MIDI ports match {requested!r}: {matches}")
    return matches[0]


def _decode_sysex(message: mido.Message) -> str | None:
    if message.type != "sysex":
        return None
    data = tuple(message.data)
    if len(data) < len(SYSEX_HEADER) or data[: len(SYSEX_HEADER)] != SYSEX_HEADER:
        return None
    payload = data[len(SYSEX_HEADER) :]
    chars: list[str] = []
    for offset in range(0, len(payload) - 1, 2):
        high, low = payload[offset], payload[offset + 1]
        if high > 0x0F or low > 0x0F:
            continue
        value = (high << 4) | low
        chars.append("\n" if value == 0x0D else chr(value))
    return "".join(chars)


def _on_midi_message(message: mido.Message) -> None:
    global _rx_overflow
    text = _decode_sysex(message)
    if not text:
        return
    try:
        _received.put_nowait(text)
    except queue.Full:
        _rx_overflow += 1


def _encode_sysex(text: str) -> mido.Message:
    try:
        raw = text.encode("ascii")
    except UnicodeEncodeError as exc:
        raise ValueError("Shell input must contain ASCII characters only") from exc
    if len(raw) > MAX_TEXT_LENGTH:
        raise ValueError(f"Input exceeds the {MAX_TEXT_LENGTH}-byte limit")
    nibbles: list[int] = []
    for value in raw:
        nibbles.extend((value >> 4, value & 0x0F))
    return mido.Message("sysex", data=[*SYSEX_HEADER, *nibbles])


@mcp.tool()
def midi_ports() -> dict[str, list[str]]:
    """List MIDI input and output ports visible to this Python process."""
    return {"inputs": mido.get_input_names(), "outputs": mido.get_output_names()}


@mcp.tool()
def connect(port_name: str = MIDI_PORT_NAME) -> str:
    """Connect to the Yamaha keyboard's MIDI input and output ports."""
    global _input, _output
    with _lock:
        if _input is not None:
            _input.close()
            _input = None
        if _output is not None:
            _output.close()
            _output = None
        inputs = mido.get_input_names()
        outputs = mido.get_output_names()
        input_name = _find_port(inputs, port_name)
        output_name = _find_port(outputs, port_name)
        try:
            _output = mido.open_output(output_name)
            _input = mido.open_input(input_name, callback=_on_midi_message)
        except Exception:
            if _input is not None:
                _input.close()
                _input = None
            if _output is not None:
                _output.close()
                _output = None
            raise
    return f"Connected: input={input_name!r}, output={output_name!r}"


@mcp.tool()
def disconnect() -> str:
    """Close the active MIDI connections."""
    global _input, _output
    with _lock:
        if _input is not None:
            _input.close()
            _input = None
        if _output is not None:
            _output.close()
            _output = None
    return "Disconnected"


@mcp.tool()
def shell_send(text: str) -> str:
    """Send ASCII text or control characters to the diagnostic shell as SysEx."""
    message = _encode_sysex(text)
    with _lock:
        if _output is None:
            raise RuntimeError("Not connected. Call connect first.")
        _output.send(message)
    return f"Sent {len(text.encode('ascii'))} ASCII byte(s)"


@mcp.tool()
def shell_read(timeout_seconds: float = 1.0, max_chars: int = 12000) -> dict[str, Any]:
    """Read terminal text received from the keyboard for up to the timeout."""
    if not 0 <= timeout_seconds <= 60:
        raise ValueError("timeout_seconds must be between 0 and 60")
    if not 1 <= max_chars <= 50000:
        raise ValueError("max_chars must be between 1 and 50000")
    deadline = time.monotonic() + timeout_seconds
    chunks: list[str] = []
    total = 0
    while total < max_chars:
        wait = max(0.0, deadline - time.monotonic())
        try:
            chunk = _received.get(timeout=wait) if wait else _received.get_nowait()
        except queue.Empty:
            break
        remaining = max_chars - total
        chunks.append(chunk[:remaining])
        total += min(len(chunk), remaining)
        if len(chunk) > remaining:
            # Preserve the unread tail for the next call.
            tail = chunk[remaining:]
            try:
                _received.put_nowait(tail)
            except queue.Full:
                pass
            break
    global _rx_overflow
    overflow = _rx_overflow
    _rx_overflow = 0
    return {"text": "".join(chunks), "dropped_messages_since_last_read": overflow}


@mcp.tool()
def shell_command(command: str, response_timeout_seconds: float = 1.0) -> dict[str, Any]:
    """Send one shell command followed by Enter, then collect its response. See PSR_Shell_usage.md for more info"""
    if "\r" in command or "\n" in command:
        raise ValueError("Pass one command without line breaks; Enter is appended automatically")
    shell_send(command + "\r")
    return shell_read(response_timeout_seconds)


if __name__ == "__main__":
    mcp.run()
