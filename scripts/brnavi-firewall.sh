#!/bin/sh
set -eu

command -v nft >/dev/null 2>&1 || {
    echo "nftables is required to protect the gateway Control Plane" >&2
    exit 1
}

allow_file=/etc/brnavi/mongodb-allowed-cidr
if [ ! -r "$allow_file" ]; then
    echo "MongoDB source allowlist is missing: $allow_file" >&2
    exit 1
fi

mongodb_allow_rules=
allow_count=0
while IFS= read -r mongodb_allowed_cidr || [ -n "$mongodb_allowed_cidr" ]; do
    [ -n "$mongodb_allowed_cidr" ] || continue
    case "$mongodb_allowed_cidr" in
        *[!0-9./]*)
            echo "MongoDB source allowlist must contain IPv4 CIDRs, one per line" >&2
            exit 1
            ;;
    esac
    allow_count=$((allow_count + 1))
    mongodb_allow_rules="${mongodb_allow_rules}        ip saddr ${mongodb_allowed_cidr} tcp dport 27017 accept
"
done < "$allow_file"

if [ "$allow_count" -eq 0 ]; then
    echo "MongoDB source allowlist is empty: $allow_file" >&2
    exit 1
fi

nft list table inet brnavi_guard >/dev/null 2>&1 && nft delete table inet brnavi_guard

nft -f - <<NFT
table inet brnavi_guard {
    chain input {
        type filter hook input priority -10; policy accept;
        iifname "lo" accept
$mongodb_allow_rules        tcp dport 27017 drop
        tcp dport 8500 drop
        tcp dport { 8443, 8883, 5671 } accept
        udp dport 8443 accept
        meta l4proto { 33, 132, 136 } accept
    }
}
NFT
