#!/usr/bin/env python3
"""Linux terminal smoke test with a fake Steam backend and browser launcher.

Usage: python3 scripts/test-hltb-terminal.py target/release/aurelia-tui
Needs no Steam login and uses a seeded HLTB cache. The existing ProtonDB lookup
may still make a background request; no real browser or game is launched.
"""
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import sys
import tempfile
import termios
import time


def run(binary, action, fail_launcher):
    with tempfile.TemporaryDirectory(prefix="aurelia-hltb-terminal-") as directory:
        root = Path(directory)
        game = {
            "id": 400, "name": "Portal", "developer": "Valve", "publisher": "Valve",
            "homepage": "https://store.steampowered.com/app/400", "game_type": "Game",
            "icon_url": None, "cover_url": None, "installed": True,
            "install_dir": "/games/Portal", "update_available": False, "size": 0,
        }
        (root / "games.json").write_text(json.dumps([game]))
        if action == "disabled":
            (root / "config.json").write_text(json.dumps({
                "default_user": "", "hidden_games": [], "favorite_games": [],
                "allowed_games": ["Game", "DLC"], "highlight": {"Rgb": [102, 192, 244]},
                "hltb_enabled": False,
            }))
        (root / "hltb").mkdir()
        (root / "hltb/400.json").write_text(json.dumps({
            "version": 1, "steam_id": 400, "fetched_at": int(time.time()),
            "data": {"game_id": 7230, "game_name": "Portal", "main": 11230,
                     "extra": 18943, "completionist": 37927},
        }))
        backend = root / "fake-aurelia"
        backend.write_text(f"#!{sys.executable}\n" + '''
import json, sys
if '--version' in sys.argv:
    print('aurelia 0.1.40')
elif sys.argv[1] == 'login':
    print(json.dumps({'logged_in': True, 'account': 'Terminal test'}))
elif sys.argv[1] == 'info':
    print(json.dumps({'app_id': 400, 'name': 'Portal', 'developers': ['Valve'],
                      'publishers': ['Valve'], 'description': 'Aperture Science offers puzzles.'}))
elif sys.argv[1] == 'config':
    print(json.dumps({'error': 'fixture launcher settings unavailable'}))
    sys.exit(1)
else:
    print('{}')
''')
        backend.chmod(0o755)
        opener = root / "xdg-open"
        opener.write_text(
            f"#!{sys.executable}\nimport sys\nfrom pathlib import Path\n"
            f"Path({str(root / 'opened-url')!r}).write_text(sys.argv[1])\n"
            f"sys.exit({int(fail_launcher)})\n"
        )
        opener.chmod(0o755)
        pid, terminal = pty.fork()
        if pid == 0:
            environment = dict(os.environ, AURELIA_BIN=str(backend),
                               AURELIA_TUI_DIR=str(root), AURELIA_TUI_CACHE_DIR=str(root),
                               TERM="xterm-256color", PATH=f"{root}:{os.environ['PATH']}")
            os.execve(binary, [binary], environment)
        fcntl.ioctl(terminal, termios.TIOCSWINSZ, struct.pack("HHHH", 32, 120, 0, 0))
        output = bytearray()

        def read_until(predicate):
            deadline = time.monotonic() + 8
            while time.monotonic() < deadline:
                if select.select([terminal], [], [], 0.1)[0]:
                    try:
                        output.extend(os.read(terminal, 65536))
                    except OSError:
                        break
                text = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", output)
                text = text.replace(b" ", b"")
                if predicate(text):
                    return
            raise AssertionError(f"terminal check failed: {bytes(output[-1500:])!r}")

        def config_data():
            # A concurrent fs::write may briefly expose an empty config file.
            try:
                return json.loads((root / "config.json").read_text())
            except (FileNotFoundError, json.JSONDecodeError):
                return {}

        def escape():
            # Send Escape separately so a terminal parser cannot interpret the
            # following letter as one Alt+letter event.
            os.write(terminal, b"\x1b")
            time.sleep(0.1)

        try:
            if action == "disabled":
                read_until(lambda text: b"Aperture" in text)
                os.write(terminal, b"t")
                read_until(lambda text: b"HLTBisdisabled" in text)
                assert not (root / "opened-url").exists()
                assert config_data().get("hltb_enabled") is False
                print("PASS: startup disabled, cached browser action blocked")
                return
            read_until(lambda text: all(value in text for value in
                                       [b"3h7m", b"5h16m", b"10h32m", b"Aperture"]))
            if action == "toggle":
                # Local settings must work even though the fake launcher fails
                # config reads. While editing a proxy, h remains ordinary text.
                os.write(terminal, b"p")
                read_until(lambda text: b"HowLongToBeat" in text and b"Launchersettingsunavailable" in text)
                os.write(terminal, b"eh")
                read_until(lambda text: b"h_" in text)
                assert config_data().get("hltb_enabled") is True
                escape()
                os.write(terminal, b"h")
                read_until(lambda _: config_data().get("hltb_enabled") is False)
                escape()
                os.write(terminal, b"t")
                read_until(lambda text: b"HLTBisdisabled" in text)
                assert not (root / "opened-url").exists()
                # Other settings writes must retain the switch's new value;
                # this catches an out-of-date config copy in the event loop.
                os.write(terminal, b"f")
                read_until(lambda _: 400 in config_data().get("favorite_games", []))
                assert config_data().get("hltb_enabled") is False
                os.write(terminal, b"ph")
                read_until(lambda _: config_data().get("hltb_enabled") is True)
                escape()
                os.write(terminal, b"t")
            elif action == "menu":
                os.write(terminal, b"\r")
                read_until(lambda text: b"HowLongToBeatpage" in text)
                os.write(terminal, b"howlongtobeat\r")
            else:
                os.write(terminal, b"t")
            read_until(lambda _: (root / "opened-url").exists())
            assert (root / "opened-url").read_text() == "https://howlongtobeat.com/game/7230"
            if fail_launcher:
                read_until(lambda text: b"Browserlauncherexited" in text)
            print(f"PASS: {action}, launcher {'failure notice' if fail_launcher else 'success'}")
        finally:
            # A failed assertion may leave an editor or overlay open. Escape
            # both before q so teardown does not type q into the edit buffer.
            escape()
            escape()
            os.write(terminal, b"q")
            deadline = time.monotonic() + 3
            while time.monotonic() < deadline:
                exited, status = os.waitpid(pid, os.WNOHANG)
                if exited:
                    break
                if select.select([terminal], [], [], 0.05)[0]:
                    try:
                        os.read(terminal, 65536)
                    except OSError:
                        pass
            else:
                os.kill(pid, signal.SIGTERM)
                os.waitpid(pid, 0)
                raise AssertionError("TUI did not exit after q")
            os.close(terminal)
            assert os.WIFEXITED(status) and os.WEXITSTATUS(status) == 0, f"TUI exit status: {status}"


if __name__ == "__main__":
    if not sys.platform.startswith("linux"):
        raise SystemExit("This smoke test uses Linux ptys and an xdg-open stub.")
    executable = str(Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/aurelia-tui").resolve())
    for method in ["shortcut", "menu"]:
        for fails in [False, True]:
            run(executable, method, fails)
    run(executable, "toggle", False)
    run(executable, "disabled", False)
