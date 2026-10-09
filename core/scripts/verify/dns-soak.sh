#!/bin/sh
gateway=$1
workload_pid=$2
name=${3:-dns-api.app.internal}
qname=""
IFS=.
for label in $name; do
    qname="$qname\\$(printf %03o ${#label})$label"
done
unset IFS
length=$((${#name}+18))
rm -f /tmp/dns-soak.*
mkfifo /tmp/dns-soak.tcp-in
socat STDIO "TCP:$gateway:53" </tmp/dns-soak.tcp-in >/tmp/dns-soak.tcp-out 2>/tmp/dns-soak.tcp-err &
query() {
    printf "\\$(printf %03o $((length/256)))\\$(printf %03o $((length%256)))\\$(printf %03o $(($1/256)))\\$(printf %03o $(($1%256)))\\001\\000\\000\\001\\000\\000\\000\\000\\000\\000$qname\\000\\000\\001\\000\\001"
}
(
    exec 3>/tmp/dns-soak.tcp-in
    n=0
    while [ ! -e /tmp/dns-soak.stop ]; do
        n=$((n+1))
        query "$n" >&3
        sleep 0.05
    done
    echo "$n" >/tmp/dns-soak.tcp-sent
    sleep 1
    exec 3>&-
    touch /tmp/dns-soak.tcp-done
) &
dig_loop() {
    n=0
    while [ ! -e /tmp/dns-soak.stop ]; do
        n=$((n+1))
        answer=$($2 dig @"$3" "$name" A +time=1 +tries=1 +short 2>&1) && [ -n "$answer" ] \
            || echo "$(date +%T.%N) $1#$n: $answer" >>"/tmp/dns-soak.$1-failed"
        sleep 0.05
    done
    echo "$n" >"/tmp/dns-soak.$1-sent"
    touch "/tmp/dns-soak.$1-done"
}
dig_loop udp "" "$gateway" &
if [ -n "$workload_pid" ]; then
    dig_loop workload "nsenter --target $workload_pid --net" 127.0.0.11 &
else
    echo 0 >/tmp/dns-soak.workload-sent
    touch /tmp/dns-soak.workload-done
fi
