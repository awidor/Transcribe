import os
from pathlib import Path
import sys
import termios
import tty

# Run directly as the terminal's child; received text can never execute commands.
directory = Path(sys.argv[1])
original = termios.tcgetattr(sys.stdin.fileno())
try:
    tty.setraw(sys.stdin.fileno())
    if "--bracketed" in sys.argv:
        os.write(sys.stdout.fileno(), b"\x1b[?2004h")
    if "--kitty" in sys.argv:
        os.write(sys.stdout.fileno(), b"\x1b[>31u")
    os.write(sys.stdout.fileno(), b"Transcribe terminal paste test\r\n")
    (directory / "ready").touch()
    with (directory / "received").open("wb", buffering=0) as received:
        while data := os.read(sys.stdin.fileno(), 4096):
            received.write(data)
finally:
    termios.tcsetattr(sys.stdin.fileno(), termios.TCSANOW, original)
