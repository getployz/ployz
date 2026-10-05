#!/usr/bin/env bash
# cap.sh FILE.md "TITLE" -- CMD ARGS...
# Appends one capture of CMD to FILE.md and its terminal rendering to FILE.ansi.
#   CAP_TTY=1 (default)  a pipe run (exit code, stdout and stderr split) into
#                        FILE.md, then a rerun under `script` into FILE.ansi.
#   CAP_TTY=0            the pipe run only, for commands that change state.
#   CAP_TTY=only         one run under `script`, raw into FILE.ansi and
#                        ANSI-stripped into FILE.md, for state-changing commands
#                        whose live rendering matters.
#   CAP_MUST=1           exit with CMD's status when it fails, for a step later
#                        captures depend on. Otherwise the status is only recorded.
# Ctrl-C is fatal: `script` and ployz both swallow SIGINT, so cap.sh exits 130
# when it sees the interrupt, an exit of 130 or script's "Session terminated".
# Env vars set by the caller (HOME, PLOYZ_STORE, ...) pass through.
set -euo pipefail
shopt -s inherit_errexit
trap 'exit 130' INT
trap 'exit 143' TERM
md=$1 title=$2; shift 3
ansi=${md%.md}.ansi
tmp=$(mktemp -d) || exit 1
trap 'rm -rf "$tmp"' EXIT
printf -v realcmd '%q ' "$@"; cmdline=${realcmd//${PLOYZ_BIN:-}/ployz}
code=0 tty=0
if [ "${CAP_TTY:-1}" = only ]; then
  script -qec "$realcmd" /dev/null </dev/null >"$tmp/raw" 2>&1 || code=$?
  tty=$code
  printf '# exit %s\r\n' "$code" >>"$tmp/raw"
  { echo "=== $title :: \$ ${cmdline% }"; cat "$tmp/raw"; echo; } >>"$ansi"
  {
    echo "### $title (TTY)"; echo; echo '```console'; echo "\$ ${cmdline% }"; echo '```'; echo
    echo 'TTY rendering (ANSI stripped; carriage-return redraws shown as separate lines; raw in .ansi):'
    echo '```'
    sed -e 's/\x1b\[[0-9;?]*[a-zA-Z]//g' -e 's/\r$//' -e 's/\r/\n/g' "$tmp/raw"
    echo '```'; echo
  } >>"$md"
else
  "$@" </dev/null >"$tmp/stdout" 2>"$tmp/stderr" || code=$?
  {
    echo "### $title"; echo; echo '```console'; echo "\$ ${cmdline% }"; echo "# exit $code"; echo '```'
    if [ -s "$tmp/stdout" ]; then echo; echo 'stdout:'; echo '```'; cat "$tmp/stdout"; [ -z "$(tail -c1 "$tmp/stdout")" ] || echo; echo '```'; fi
    if [ -s "$tmp/stderr" ]; then echo; echo 'stderr:'; echo '```'; cat "$tmp/stderr"; [ -z "$(tail -c1 "$tmp/stderr")" ] || echo; echo '```'; fi
    echo
  } >>"$md"
  if [ "${CAP_TTY:-1}" = 1 ]; then
    script -qec "$realcmd" /dev/null </dev/null >"$tmp/raw" 2>&1 || tty=$?
    { echo "=== $title :: \$ ${cmdline% }"; cat "$tmp/raw"; echo; } >>"$ansi"
  fi
fi
interrupted=0
if [ "$code" = 130 ] || [ "$tty" = 130 ]; then interrupted=1; fi
if [ -e "$tmp/raw" ] && grep -q 'Session terminated' "$tmp/raw"; then interrupted=1; fi
rm -rf "$tmp"
if [ "$interrupted" = 1 ]; then
  echo "interrupted during: $title" >&2
  exit 130
fi
if [ "${CAP_MUST:-0}" = 1 ] && [ "$code" != 0 ]; then
  echo "required step failed with exit $code: $title" >&2
  exit "$code"
fi
