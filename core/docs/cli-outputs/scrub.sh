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
  # Global unicast IPv6 (2000::/3), as a Boat Server reports for its public IP.
  s/(?<![0-9A-Fa-f:])([23][0-9A-Fa-f]{3}(?::[0-9A-Fa-f]{0,4}){2,7})(?![0-9A-Fa-f:])/do { my $ip = $1; ($ip =~ m{::} || ($ip =~ tr{:}{}) == 7) && $ip !~ m{^2001:db8:}i ? "2001:db8::10" : $ip }/ge;
  # Secrets are a prefix plus 43 base64url characters: tokens (ployz_), Server
  # enrollment tokens (pmet_) and pairing secrets (ppair_).
  s/(ployz|pmet|ppair)_[A-Za-z0-9_-]{40,}/$1_<redacted>/g;
' "$@"
