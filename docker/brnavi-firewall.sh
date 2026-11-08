#!/bin/sh
set -eu

command -v nft >/dev/null 2>&1 || {
    echo "nftables is required to protect the gateway Control Plane" >&2
    exit 1
}

if nft list table inet brnavi_guard >/dev/null 2>&1; then
    nft delete table inet brnavi_guard
fi

nft -f - <<'NFT'
table inet brnavi_guard {
    chain input {
        type filter hook input priority -10; policy accept;
        iifname "lo" accept
        iifname "br-*" ip saddr 172.16.0.0/12 tcp dport 8500 accept
        tcp dport 8500 drop
        tcp dport { 8443, 8883, 5671 } accept
        udp dport 8443 accept
        meta l4proto 33 accept
        meta l4proto 132 accept
        meta l4proto 136 accept
    }
}
NFT
