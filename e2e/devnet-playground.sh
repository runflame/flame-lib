#!/usr/bin/env bash
# Checks that a node following a mint playground issues flames for the
# playground's burns. A fresh wallet B burns in the playground as its only
# minter; every Flame block that moves to later playground blocks pays B
# their 50 FLM allotments, and B can spend them. Exits non-zero on the
# first thing that is wrong. Run from anywhere:
#
#   e2e/devnet-playground.sh
#
# KEEP=1, PORT (default 18548) and CARGO_BUILD_JOBS as in devnet.sh;
# PLAYGROUND_PORT (default 18549) is where the playground listens.
port=18548
playground_port="${PLAYGROUND_PORT:-18549}"
playground="http://127.0.0.1:$playground_port"

# The playground has its own workspace, so its own build and target dir.
before_node() {
    local app="$root/demos/mint-playground"
    cargo build --manifest-path "$app/Cargo.toml"
    MINT_LISTEN="127.0.0.1:$playground_port" "$app/target/debug/mint-playground" \
        "$dir/mint.sqlite3" >"$dir/playground.log" 2>&1 &
    extra_pids+=($!)
    for _ in $(seq 100); do
        grep -q "Mint playground:" "$dir/playground.log" 2>/dev/null && break
        sleep 0.2
    done
    grep -q "Mint playground:" "$dir/playground.log" ||
        { cat "$dir/playground.log" >&2; echo "FAIL: the playground never served" >&2; exit 1; }
    node_toml="playground_url = \"$playground\""
}
source "$(dirname "${BASH_SOURCE[0]}")/devnet.sh"

a=(--wallet "$dir/a.json")
b=(--wallet "$dir/b.json")
fifty=5000000000

rpc() {
    curl -sf "$FLAMEWALLET_NODE" -H 'content-type: application/json' \
        -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$1\",\"params\":$2}"
}
tip_height() { rpc tip '[]' | field "['result']['height']"; }
core_height() { rpc block "[$1]" | field "['result']['summary']['header']['core_height']"; }
# How many of a block's executions are mints.
mints() {
    rpc block "[$1]" | python3 -c "
import json, sys
print(sum(e['execution']['kind'] == 'mint' for e in json.load(sys.stdin)['result']['executions']))"
}

printf 'a1%.0s' $(seq 64) | "$wallet" "${a[@]}" restore --seed >/dev/null
"$wallet" "${b[@]}" new >/dev/null 2>&1
a_address="$("$wallet" "${a[@]}" --json address | field "[0]['address']")"
b_address="$("$wallet" "${b[@]}" --json address | field "[0]['address']")"

say "B burns 1,000,000 sat over 144 playground blocks"
start="$(curl -sf "$playground/api/contributions" -H 'content-type: application/json' \
    -H "origin: $playground" \
    -d "{\"address\":\"$b_address\",\"budget_sats\":1000000,\"duration\":144,\"request_id\":\"e2e-0000000000000001\"}" |
    field "['start_block']")"
echo "   from playground block $start"

say "Flame blocks issue B's allotments"
for _ in $(seq 60); do
    [[ "$(balance FLM "${b[@]}")" != 0 ]] && break
    sleep 1
done
# A balance and the core height it is for, read at one tip: the node mints
# every two seconds, so read until the tip holds still around both.
for _ in $(seq 10); do
    tip="$(tip_height)"
    held="$(balance FLM "${b[@]}")"
    [[ "$(tip_height)" == "$tip" ]] && break
done
core="$(core_height "$tip")"
paid=$((core - start + 1))
echo "   tip $tip on playground block $core: $paid block(s) of B's burns"
(( paid > 0 )) || { echo "FAIL: no playground block of B's was paid" >&2; exit 1; }
expect "B's minted flames" "$held" "$((paid * fifty))"
minted=0
for h in $(seq 1 "$tip"); do minted=$((minted + $(mints "$h"))); done
(( minted > 0 )) || { echo "FAIL: no block has a mint execution" >&2; exit 1; }
echo "   $minted block(s) with a mint execution"
expect "A is untouched" "$(balance FLM "${a[@]}")" 100000000000

say "B spends a minted flame"
"$wallet" "${b[@]}" send "$a_address" 1 --wait >/dev/null
expect "A after B pays 1 FLM" "$(balance FLM "${a[@]}")" 100100000000

echo
echo "devnet playground: ok"
