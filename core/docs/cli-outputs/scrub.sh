#!/usr/bin/env bash
set -euo pipefail
shopt -s inherit_errexit
perl -pi -e '
  sub public {
    my ($a, $b, $c) = @_;
    return 0 if grep { $_ > 255 } @_;
    return 0 if $a == 0 || $a == 10 || $a == 127 || $a >= 224;
    return 0 if ($a == 172 && $b >= 16 && $b <= 31) || ($a == 192 && $b == 168);
    return 0 if ($a == 169 && $b == 254) || ($a == 100 && $b >= 64 && $b <= 127);
    return 0 if "$a.$b.$c" =~ /^(192\.0\.2|198\.51\.100|203\.0\.113)$/;
    1;
  }
  s/(?<![\d.])(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})(?!\d|\.\d)/public($1, $2, $3, $4) ? "203.0.113.10" : $&/ge;
  # Secrets are a prefix plus 43 base64url characters: tokens (ployz_), Server
  # enrollment tokens (pmet_) and pairing secrets (ppair_).
  s/(ployz|pmet|ppair)_[A-Za-z0-9_-]{40,}/$1_<redacted>/g;
' "$@"
