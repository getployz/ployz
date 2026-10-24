"""Runs the Shell install line's `sh -s -- VERSION --enroll` with the script on a pipe, as `curl | sh` does,
on a real terminal that types the Fleet Token the instant the prompt shows, and with no terminal at all.
It also cancels once at each syscall the shell makes between showing the prompt and blocking on the terminal, and
between the Enter that ends the token and the terminal getting its echo back. And it runs the shell as sudo-rs does
for `curl | sudo sh`: in the background of the terminal, resuming only the shell when the terminal stops it."""

import ctypes
import hashlib
import itertools
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
PROMPT = b'Fleet Token (from Servers in Ployz Cloud): '
WRITE_SYSCALL = {'x86_64': 1, 'aarch64': 64}[os.uname().machine]
PTRACE_TRACEME, PTRACE_DETACH, PTRACE_SYSCALL, PTRACE_SETOPTIONS, PTRACE_SEIZE, PTRACE_INTERRUPT = \
    0, 17, 24, 0x4200, 0x4206, 0x4207
PTRACE_O_TRACESYSGOOD, PTRACE_O_TRACEEXEC, PTRACE_O_EXITKILL = 0x1, 0x10, 0x100000
libc = ctypes.CDLL(None, use_errno=True)
libc.ptrace.argtypes = [ctypes.c_long, ctypes.c_long, ctypes.c_void_p, ctypes.c_void_p]


def ptrace(request, pid, data=0):
    if libc.ptrace(request, pid, None, data or None) == -1:
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error))


def syscall_args(pid):
    return [int(field, 0) for field in Path(f'/proc/{pid}/syscall').read_text().split()[:4]]


def writes_prompt(pid):
    number, _, buffer, length = syscall_args(pid)
    if number != WRITE_SYSCALL:
        return False
    with open(f'/proc/{pid}/mem', 'rb') as memory:
        memory.seek(buffer)
        return PROMPT in memory.read(length)


def time_out(*_):
    raise TimeoutError


signal.signal(signal.SIGALRM, time_out)


def pending(pid, number):
    fields = dict(line.split(':', 1) for line in Path(f'/proc/{pid}/status').read_text().splitlines())
    return any(int(fields[key], 16) >> (number - 1) & 1 for key in ('SigPnd', 'ShdPnd'))


def stub(path, text):
    path.write_text(text)
    path.chmod(0o755)


def exec_like_sudo_rs(shell, env):
    """Starts the shell in its own process group in the background of this terminal. When the terminal stops it, this
    hands it the terminal and resumes the shell alone, never the children the stop also caught, as sudo-rs 0.2 does."""
    child = os.fork()
    if child == 0:
        os.setpgid(0, 0)
        os.execve(shell, [shell, *ARGS], env)
    signal.signal(signal.SIGTTOU, signal.SIG_IGN)
    while True:
        _, status = os.waitpid(child, os.WUNTRACED)
        if not os.WIFSTOPPED(status):
            os._exit(os.waitstatus_to_exitcode(status))
        os.tcsetpgrp(1, child)
        os.kill(child, signal.SIGCONT)


def run(shell, env, mode, sudo_rs=False):
    script_read, script_write = os.pipe()
    pid, master = pty.fork()
    if pid == 0:
        os.close(script_write)
        os.dup2(script_read, 0)
        os.close(script_read)
        if sudo_rs:
            exec_like_sudo_rs(shell, env)
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

    wait_for('prompt', lambda: PROMPT in output)
    echo_at_prompt = bool(termios.tcgetattr(master)[3] & termios.ECHO)
    if mode == 'term':
        os.kill(pid, signal.SIGTERM)
    else:
        os.write(master, {'type': TOKEN + '\n', 'ctrl-c': '\x03'}[mode].encode())
    wait_for('exit', exited)
    echo_after = bool(termios.tcgetattr(master)[3] & termios.ECHO)
    os.close(master)
    rc = os.waitstatus_to_exitcode(status)
    assert not echo_at_prompt, f'{shell} {mode}: echo was still on when the prompt showed'
    assert TOKEN.encode() not in output, f'{shell} {mode}: the token echoed: {output!r}'
    assert echo_after, f'{shell} {mode}: echo stayed off after the install'
    return rc


def cancel_mid_prompt(shell, env, mode, after, nth):
    """Holds the shell at the nth distinct syscall it starts after writing the prompt, or after the token's Enter, as a
    busy CPU can, and cancels once there. Returns whether that was the last syscall worth holding at, and what went wrong."""
    number = {'term': signal.SIGTERM, 'ctrl-c': signal.SIGINT}[mode]
    script_read, script_write = os.pipe()
    pid, master = pty.fork()
    if pid == 0:
        os.close(script_write)
        os.dup2(script_read, 0)
        os.close(script_read)
        if after == 'prompt':
            libc.ptrace(PTRACE_TRACEME, 0, None, None)
            os.kill(os.getpid(), signal.SIGSTOP)
        os.execve(shell, [shell, *ARGS], env)
    os.close(script_read)
    os.write(script_write, SOURCE)
    os.close(script_write)

    def next_stop(timeout):
        signal.setitimer(signal.ITIMER_REAL, timeout)
        try:
            return os.waitpid(pid, 0)[1]
        except TimeoutError:
            return None
        finally:
            signal.setitimer(signal.ITIMER_REAL, 0)

    options = PTRACE_O_TRACESYSGOOD | PTRACE_O_TRACEEXEC | PTRACE_O_EXITKILL
    if after == 'prompt':
        os.waitpid(pid, 0)
        ptrace(PTRACE_SETOPTIONS, pid, options)
    else:
        output, deadline = b'', time.monotonic() + 10
        while PROMPT not in output:
            assert time.monotonic() < deadline, f'{shell} {mode}: no prompt; output: {output!r}'
            if select.select([master], [], [], .01)[0]:
                output += os.read(master, 65536)
        ptrace(PTRACE_SEIZE, pid, options)
        ptrace(PTRACE_INTERRUPT, pid)
        assert next_stop(10) is not None, f'{shell} {mode}: never stopped for the tracer'
    entering, prompted, typed, seen, forward, held, blocked = True, after == 'enter', False, 0, 0, None, False
    previous = None
    while seen < nth:
        ptrace(PTRACE_SYSCALL, pid, forward)
        forward = 0
        status = next_stop(.5 if prompted and not typed else 10)
        if status is None:
            assert prompted and not typed, f'{shell} {mode}: stuck {"after Enter" if typed else "before the prompt"}'
            if after == 'prompt':
                blocked = True
                break
            os.write(master, (TOKEN + '\n').encode())
            typed = True
            status = next_stop(10)
            assert status is not None, f'{shell} {mode}: never woke for the Enter'
        if os.WIFEXITED(status) and typed:
            os.close(master)
            return True, None
        assert os.WIFSTOPPED(status), f'{shell} {mode}: exited before it was cancelled'
        stop = os.WSTOPSIG(status)
        if stop == signal.SIGTRAP | 0x80:
            if entering and prompted and (typed or after == 'prompt'):
                # dash reads a pipe a byte per syscall: a run of the same call on the same fd is one place to hold.
                current = syscall_args(pid)[:2]
                seen += current != previous
                held, previous = current[0], current
            elif entering and not prompted:
                prompted = writes_prompt(pid)
            entering = not entering
        elif stop != signal.SIGTRAP:
            forward = stop
    echo_at_cancel = bool(termios.tcgetattr(master)[3] & termios.ECHO)
    last = blocked or echo_at_cancel and typed
    if mode == 'term':
        os.kill(pid, number)
    else:
        os.write(master, b'\x03')
    where = f'while blocked in syscall {held}' if blocked else f'at syscall {held}, the {nth} after {after}'
    if blocked:
        while (stop := os.WSTOPSIG(next_stop(10) or 0)) != number:
            assert stop, f'{shell} {mode}: the signal never reached the shell'
            ptrace(PTRACE_SYSCALL, pid, 0 if stop in (signal.SIGTRAP, signal.SIGTRAP | 0x80) else stop)
        ptrace(PTRACE_DETACH, pid, number)
    else:
        deadline = time.monotonic() + 10
        while not pending(pid, number):
            assert time.monotonic() < deadline, f'{shell} {mode}: the signal never reached the shell'
            time.sleep(.001)
        ptrace(PTRACE_DETACH, pid)
    status = next_stop(10)
    echo_after = bool(termios.tcgetattr(master)[3] & termios.ECHO)
    os.close(master)
    if status is None:
        os.kill(pid, signal.SIGKILL)
        os.waitpid(pid, 0)
        return last, f'{shell} {mode} {where}: still running 10s later, echo {"on" if echo_after else "off"}'
    rc = os.waitstatus_to_exitcode(status)
    if not echo_after or not typed and (echo_at_cancel or rc != 130):
        return last, f'{shell} {mode} {where}: exit {rc}, echo at cancel {echo_at_cancel}, after {echo_after}'
    return last, None


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
    # A throwaway key stands in for the release key.
    subprocess.run(['minisign', '-G', '-W', '-p', root / 'test.pub', '-s', root / 'test.key'],
                   check=True, stdout=subprocess.DEVNULL)
    subprocess.run(['minisign', '-S', '-l', '-s', root / 'test.key', '-m', root / 'release/checksums.txt',
                    '-x', root / 'release/checksums.txt.minisig', '-t', 'ployz v1.2.3'],
                   check=True, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL)
    stub(root / 'bin/curl', '''#!/bin/sh
write_out=
while [ "$#" -gt 0 ]; do
    case "$1" in -o) output=$2; shift 2 ;; -w) write_out=1; shift 2 ;; *) url=$1; shift ;; esac
done
cp "$FAKE_RELEASE/${url##*/}" "$output"
[ -z "$write_out" ] || printf 200
''')
    stub(root / 'bin/id', '#!/bin/sh\necho 0\n')
    stub(root / 'bin/uname', '#!/bin/sh\ncase "$1" in -s) echo Linux ;; -m) echo x86_64 ;; esac\n')
    stub(root / 'bin/systemctl', '#!/bin/sh\n')
    env = {key: value for key, value in os.environ.items() if key not in ('PLOYZ_ENROLL_TOKEN', 'PLOYZ_FLEET_TOKEN')}
    env.pop('PLOYZ_VERSION', None)
    env.update(PATH=f'{root}/bin:{env["PATH"]}', PLOYZ_TEST_ROOT=tmp,
               INSTALL_BIN_DIR=str(root / 'install'), FAKE_RELEASE=str(root / 'release'),
               PLOYZ_GITHUB_URL='https://example.invalid',
               PLOYZ_RELEASE_PUBLIC_KEY=(root / 'test.pub').read_text().splitlines()[1], PROBE_ENV=str(root / 'env'),
               PROBE_ARGV=str(root / 'argv'), PROBE_STDIN=str(root / 'stdin'))

    shells = [shell for shell in ['/bin/sh', shutil.which('dash'), shutil.which('bash')] if shell]
    late_cancels = []
    for shell in dict.fromkeys(shells):
        for attempt in range(5):
            assert run(shell, env, 'type') == 0, shell
            assert (root / 'stdin').read_text() == TOKEN + '\n', shell
            assert TOKEN not in (root / 'env').read_text(), shell
            assert TOKEN not in (root / 'argv').read_text(), shell
        assert run(shell, env, 'type', sudo_rs=True) == 0, shell
        assert (root / 'stdin').read_text() == TOKEN + '\n', shell
        assert run(shell, env, 'ctrl-c') == 130, shell
        assert run(shell, env, 'term') == 130, shell
        for after, mode in itertools.product(['prompt', 'enter'], ['ctrl-c', 'term']):
            for nth in itertools.count(1):
                last, failure = cancel_mid_prompt(shell, env, mode, after, nth)
                if failure:
                    late_cancels.append(failure)
                if failure or last:
                    break
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
    assert not late_cancels, 'one cancel at the prompt did not end the install with echo back on:\n' + '\n'.join(late_cancels)
    print('install.sh keeps the Fleet Token off the terminal in', ', '.join(dict.fromkeys(shells)))
