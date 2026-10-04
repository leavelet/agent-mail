#!/usr/bin/env python3
"""A stand-in for a harness TUI, for the terminal-paste tests: it draws an
idle screen (an input box between two rules, or a dialog), turns on
bracketed paste like real TUIs do, and records every byte it receives and
every line Enter submits in the file named by its first argument.

usage: fake_tui.py LOG MODE   (MODE: empty | draft | dialog)
"""
import os
import sys
import termios
import tty

log_path, mode = sys.argv[1], sys.argv[2]
fd = sys.stdin.fileno()
old = termios.tcgetattr(fd)
tty.setraw(fd)
buf = 'half a sentence' if mode == 'draft' else ''
log = open(log_path, 'a')


def draw():
    out = '\x1b[?2004h\x1b[2J\x1b[H● Done. The tests pass.\r\n\r\n'
    if mode == 'dialog':
        out += 'Do you want to proceed?\r\n❯ 1. Yes\r\n  2. No\r\n\r\nEsc to cancel\r\n'
    else:
        rule = '─' * 40
        out += f'{rule}\r\n> {buf}\r\n{rule}\r\n  ? for shortcuts\r\n'
    sys.stdout.write(out)
    sys.stdout.flush()


draw()
log.write('READY\n')
log.flush()
try:
    while True:
        data = os.read(fd, 4096)
        if not data:
            break
        log.write('RECV ' + repr(data) + '\n')
        text = data.replace(b'\x1b[200~', b'').replace(b'\x1b[201~', b'').decode(errors='replace')
        for c in text:
            if c in '\r\n':
                log.write(('ANSWER ' if mode == 'dialog' else 'SUBMIT ') + buf + '\n')
                buf = ''
            elif c == '\x03':
                raise SystemExit
            else:
                buf += c
        log.flush()
        draw()
finally:
    termios.tcsetattr(fd, termios.TCSADRAIN, old)
