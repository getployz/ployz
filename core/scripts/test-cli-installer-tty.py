"""Runs the Shell install line's `sh -s -- VERSION --enroll` with the script on a pipe, as `curl | sh` does,
on a real terminal that types the Fleet Token the instant the prompt shows, and with no terminal at all."""

import hashlib
import os
import pty
import select
import shutil
import signal
import subprocess
import sys
import tarfile
import tempfile
import termios
import time
from pathlib import Path

SOURCE = Path(sys.argv[1]).read_bytes()
TOKEN = 'pflt_' + 'a' * 43
ARGS = ['-s', '--', '1.2.3', '--enroll']
READ_SYSCALL = {'x86_64': '0', 'aarch64': '63'}[os.uname().machine]


def reading_terminal(pid):
    return Path(f'/proc/{pid}/syscall').read_text().split()[:2] == [READ_SYSCALL, '0x0']


def stub(path, text):
    path.write_text(text)
    path.chmod(0o755)


def run(shell, env, mode):
    script_read, script_write = os.pipe()
    pid, master = pty.fork()
    if pid == 0:
        os.close(script_write)
        os.dup2(script_read, 0)
        os.close(script_read)
        os.execve(shell, [shell, *ARGS], env)
    os.close(script_read)
    os.write(script_write, SOURCE)
    os.close(script_write)
    output = b''
    status = None

    def wait_for(phase, reached):
        nonlocal output
        start = time.monotonic()
        while not reached():
            elapsed = time.monotonic() - start
            assert elapsed < 10, f'{shell} {mode}: no {phase} after {elapsed:.1f}s; output: {output!r}'
            if select.select([master], [], [], .01)[0]:
                try:
                    output += os.read(master, 65536)
                except OSError:
                    pass

    def exited():
        nonlocal status
        done, status = os.waitpid(pid, os.WNOHANG)
        return done

    wait_for('prompt', lambda: b'Fleet Token (from Servers in Ployz Cloud): ' in output)
    echo_at_prompt = bool(termios.tcgetattr(master)[3] & termios.ECHO)
    if mode == 'type':
        os.write(master, (TOKEN + '\n').encode())
    else:
        # A trapped signal that lands after the prompt but before the shell blocks in read(2) waits for a line or a
        # second signal, in dash and bash alike. Nobody presses Ctrl-C that fast, so signal once the shell reads.
        wait_for('blocking read of the terminal', lambda: reading_terminal(pid))
        if mode == 'term':
            os.kill(pid, signal.SIGTERM)
        else:
            os.write(master, b'\x03')
    wait_for('exit', exited)
    echo_after = bool(termios.tcgetattr(master)[3] & termios.ECHO)
    os.close(master)
    rc = os.waitstatus_to_exitcode(status)
    assert not echo_at_prompt, f'{shell} {mode}: echo was still on when the prompt showed'
    assert TOKEN.encode() not in output, f'{shell} {mode}: the token echoed: {output!r}'
    assert echo_after, f'{shell} {mode}: echo stayed off after the install'
    return rc


with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp)
    for directory in ['bin', 'release', 'install', 'dev']:
        (root / directory).mkdir()
    (root / 'dev/tty').symlink_to('/dev/tty')
    binary = root / 'ployz'
    stub(binary, '''#!/bin/sh
if [ "${1:-}" = --version ]; then echo 1.2.3; exit 0; fi
env > "$PROBE_ENV"
printf '%s\\n' "$*" > "$PROBE_ARGV"
cat > "$PROBE_STDIN"
echo 'Waiting for approval'
''')
    archive = root / 'release/ployz_linux_amd64.tar.gz'
    with tarfile.open(archive, 'w:gz') as tar:
        tar.add(binary, arcname='ployz')
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    (root / 'release/checksums.txt').write_text(f'{digest}  {archive.name}\n')
    stub(root / 'bin/curl', '''#!/bin/sh
while [ "$#" -gt 0 ]; do
    if [ "$1" = -o ]; then output=$2; shift 2; else url=$1; shift; fi
done
cp "$FAKE_RELEASE/${url##*/}" "$output"
''')
    stub(root / 'bin/id', '#!/bin/sh\necho 0\n')
    stub(root / 'bin/uname', '#!/bin/sh\ncase "$1" in -s) echo Linux ;; -m) echo x86_64 ;; esac\n')
    stub(root / 'bin/systemctl', '#!/bin/sh\n')
    env = {key: value for key, value in os.environ.items() if key not in ('PLOYZ_ENROLL_TOKEN', 'PLOYZ_FLEET_TOKEN')}
    env.pop('PLOYZ_VERSION', None)
    env.update(PATH=f'{root}/bin:{env["PATH"]}', PLOYZ_TEST_ROOT=tmp,
               INSTALL_BIN_DIR=str(root / 'install'), FAKE_RELEASE=str(root / 'release'),
               PLOYZ_GITHUB_URL='https://example.invalid', PROBE_ENV=str(root / 'env'),
               PROBE_ARGV=str(root / 'argv'), PROBE_STDIN=str(root / 'stdin'))

    shells = [shell for shell in ['/bin/sh', shutil.which('dash'), shutil.which('bash')] if shell]
    for shell in dict.fromkeys(shells):
        for attempt in range(5):
            assert run(shell, env, 'type') == 0, shell
            assert (root / 'stdin').read_text() == TOKEN + '\n', shell
            assert TOKEN not in (root / 'env').read_text(), shell
            assert TOKEN not in (root / 'argv').read_text(), shell
        assert run(shell, env, 'ctrl-c') == 130, shell
        assert run(shell, env, 'term') == 130, shell
        (root / 'install/ployz').unlink()
        detached = subprocess.run([shell, *ARGS], input=SOURCE, env=env, capture_output=True, start_new_session=True)
        assert detached.returncode == 1, (shell, detached)
        assert b'No terminal to read the Fleet Token from.' in detached.stderr, (shell, detached.stderr)
        assert not (root / 'install/ployz').exists(), shell
        (root / 'stdin').unlink()
        headless = subprocess.run([shell, *ARGS], input=SOURCE, env={**env, 'PLOYZ_FLEET_TOKEN': TOKEN},
                                  capture_output=True, start_new_session=True)
        assert headless.returncode == 0, (shell, headless)
        assert b'Fleet Token (from Servers in Ployz Cloud)' not in headless.stdout + headless.stderr, shell
        assert (root / 'stdin').read_text() == TOKEN + '\n', shell
        assert TOKEN not in (root / 'argv').read_text(), shell
    print('install.sh keeps the Fleet Token off the terminal in', ', '.join(dict.fromkeys(shells)))
