#!/usr/bin/env bash
# The mint playground paying its rewards on a throwaway devnet: the genesis
# wallet funds a payout wallet, fourteen minters contribute (more than one
# payout transaction holds), the playground is killed and restarted midway,
# and once every contribution has ended, each minter's on-chain balance must
# equal both its reward and what the playground reports as paid, all of it
# in cleartext coins, while the payout wallet keeps none. Takes about
# three minutes, most of it the shortest contribution the playground takes.
#
#   e2e/devnet-mint-payout.sh
#
# KEEP=1, PORT (default 18548) and CARGO_BUILD_JOBS as in devnet.sh;
# MINT_PORT (default 13048) is the playground's.
port=18548
source "$(dirname "${BASH_SOURCE[0]}")/devnet.sh"

cargo build --manifest-path "$root/demos/mint-playground/Cargo.toml" --locked
playground="$root/demos/mint-playground/target/debug/mint-playground"
mint="http://127.0.0.1:${MINT_PORT:-13048}"
minters=14

a=(--wallet "$dir/a.json")
p=(--wallet "$dir/payout.json")
printf 'a1%.0s' $(seq 64) | "$wallet" "${a[@]}" restore --seed >/dev/null
"$wallet" "${p[@]}" --json new >"$dir/payout.new"
field "['mnemonic']" <"$dir/payout.new" >"$dir/payout.mnemonic"
payout_address="$(field "['address']" <"$dir/payout.new")"
"$wallet" "${a[@]}" send "$payout_address" 100000 --wait >/dev/null
funded="$(balance FLM "${p[@]}")"
expect "payout wallet funded" "$funded" 10000000000000

playground_pid=""
start_playground() {
    MINT_LISTEN="${mint#http://}" \
        MINT_PAYOUT_MNEMONIC_FILE="$dir/payout.mnemonic" \
        MINT_PAYOUT_NODE="$FLAMEWALLET_NODE" \
        MINT_PAYOUT_MIN_SPARKS=1 \
        "$playground" "$dir/mint.sqlite3" >>"$dir/mint.log" 2>&1 &
    playground_pid=$!
    for _ in $(seq 50); do
        curl -sf "$mint/api/rewards" >/dev/null && return
        sleep 0.2
    done
    echo "FAIL: the playground never served" >&2
    tail -n 20 "$dir/mint.log" >&2
    exit 1
}
trap 'status=$?; kill "$playground_pid" 2>/dev/null || true; (exit $status); cleanup' EXIT

say "start the playground and contribute from $minters minters"
start_playground
for i in $(seq "$minters"); do
    "$wallet" --wallet "$dir/m$i.json" new >/dev/null 2>&1
    address="$("$wallet" --wallet "$dir/m$i.json" --json address | field "[0]['address']")"
    curl -sf "$mint/api/contributions" -H 'content-type: application/json' --data \
        "{\"address\":\"$address\",\"budget_sats\":$((150000 + i * 10000)),\"duration\":144,\"request_id\":\"payout-e2e-$(printf %05d "$i")\"}" \
        >/dev/null
done

say "kill the playground mid-payout and restart it"
sleep 20
kill -9 "$playground_pid"
wait "$playground_pid" 2>/dev/null || true
start_playground

say "wait for the contributions to end and every reward to be paid"
rewards_settled() {
    curl -sf "$mint/api/rewards" | python3 -c "
import json, sys
rows = json.load(sys.stdin)
sys.exit(0 if len(rows) == $minters and all(r['paid_sparks'] == r['reward_sparks'] for r in rows) else 1)"
}
for _ in $(seq 300); do
    sleep 1
    [[ "$(curl -sf "$mint/api/state" | field "['height']")" -gt 146 ]] && rewards_settled && break
done
rewards_settled || {
    echo "FAIL: rewards not paid in time" >&2
    tail -n 20 "$dir/mint.log" >&2
    exit 1
}

say "compare the chain with the ledger"
rewards="$(curl -sf "$mint/api/rewards")"
paid_total=0
for i in $(seq "$minters"); do
    address="$("$wallet" --wallet "$dir/m$i.json" --json address | field "[0]['address']")"
    reward="$(python3 -c "
import json, sys
print(next(r['reward_sparks'] for r in json.loads(sys.argv[1]) if r['address'] == sys.argv[2]))" "$rewards" "$address")"
    expect "minter $i on chain" "$(balance FLM --wallet "$dir/m$i.json")" "$reward"
    expect "minter $i paid clear" "$(clear_held FLM --wallet "$dir/m$i.json")" "$reward"
    paid_total=$((paid_total + reward))
done
expect "payout change stays confidential" "$(clear_held FLM "${p[@]}")" 0
fees=$((funded - paid_total - $(balance FLM "${p[@]}")))
(( fees > 0 && fees % 1000 == 0 )) || { echo "FAIL: payout fees $fees" >&2; exit 1; }
grep '^Payout' "$dir/mint.log" | sed 's/^/   /'
echo "   $paid_total sparks in $((fees / 1000)) transactions"

echo "devnet mint payout: ok"
