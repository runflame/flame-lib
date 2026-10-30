#!/usr/bin/env bash
# Starts a throwaway devnet node and runs the whole DEX on it: the genesis
# wallet A deploys two coins and the FLM/EMBER pool, lists the coins,
# mints, provides liquidity; a fresh wallet B deploys an EMBER of its own,
# fails to deploy one under A's key, mints, swaps both ways and is refunded
# when its price limit is missed; A takes liquidity out and renews a lease.
# Checks balances on the way and exits non-zero on the first thing that
# is wrong. Run from anywhere:
#
#   demos/dex/e2e.sh
#
# KEEP=1, PORT (default 18548) and CARGO_BUILD_JOBS as in e2e/devnet.sh.
port=18548
source "$(dirname "${BASH_SOURCE[0]}")/../../e2e/devnet.sh"

here="$root/demos/dex"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/demos/mint-playground/target}" \
    cargo build --manifest-path "$here/Cargo.toml"
dex="${CARGO_TARGET_DIR:-$root/demos/mint-playground/target}/debug/flamedex"

a=(--wallet "$dir/a.json")
b=(--wallet "$dir/b.json")
printf 'a1%.0s' $(seq 64) | "$wallet" "${a[@]}" restore --seed >/dev/null
"$wallet" "${b[@]}" new >/dev/null 2>&1
b_address="$("$wallet" "${b[@]}" --json address | field "[0]['address']")"

say "A pays B 100 Flame"
"$wallet" "${a[@]}" send "$b_address" 100 --wait

say "A deploys the coins EMBER and ASH under its issuer key"
"$dex" "${a[@]}" deploy-coin EMBER
"$dex" "${a[@]}" deploy-coin ASH --max 1000 --cap 500
a_issuer="$("$dex" "${a[@]}" issuer)"

say "A lists them, so that EMBER and ASH name A's coins"
"$dex" "${a[@]}" describe EMBER@me "ASH@$a_issuer" >"$dir/listed.json"
python3 - "$dir/listed.json" "$dir/coins.json" <<'PY'
import json, sys
coins = json.load(open(sys.argv[1]))
coins[0]["name"] = "Ember"
json.dump({"name": "e2e", "network": "devnet", "coins": coins}, open(sys.argv[2], "w"), indent=2)
PY
export FLAMEDEX_LIST="$dir/coins.json"
ember="$(field "['coins'][0]['flavor']" <"$dir/coins.json")"

say "deploying EMBER again is refused before anything is sent"
if "$dex" "${a[@]}" deploy-coin EMBER 2>"$dir/err"; then
    echo "FAIL: a second EMBER deployed" >&2; exit 1
fi
grep -q "already deployed" "$dir/err" || { cat "$dir/err" >&2; exit 1; }

say "B deploys an EMBER of its own: another issuer, another coin"
"$dex" "${b[@]}" deploy-coin EMBER --fund 30
"$dex" "${b[@]}" mint EMBER@me 7
own="$("$dex" "${b[@]}" describe EMBER@me | field "[0]['flavor']")"
[ "$own" != "$ember" ] || { echo "FAIL: B's EMBER is A's" >&2; exit 1; }
expect "B's own EMBER" "$(balance "$own" "${b[@]}")" 7

say "B cannot deploy a coin under A's key: the constructor checks the signature"
"$dex" "${b[@]}" deploy-coin SQUAT --fund 30 --forge-issuer "$a_issuer"
"$dex" status "SQUAT@$a_issuer" | grep -q "not deployed" \
    || { echo "FAIL: a coin was deployed under A's key without A" >&2; exit 1; }
python3 - "$FLAMEWALLET_NODE" <<'PY' || { echo "FAIL: the squat did not fail on its signature" >&2; exit 1; }
import json, sys, urllib.request
def rpc(method, params):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    req = urllib.request.Request(sys.argv[1], body, {"content-type": "application/json"})
    return json.load(urllib.request.urlopen(req))["result"]
tip = rpc("tip", [])["height"]
errors = [x["execution"].get("error", "") for h in range(max(1, tip - 3), tip + 1)
          for x in rpc("block", [h])["executions"]]
print("bounced:", [e for e in errors if e])
assert any("ignature" in e for e in errors)
PY

say "A mints 10000 EMBER, B mints 300"
"$dex" "${a[@]}" mint EMBER 10000
"$dex" "${b[@]}" mint EMBER 300
expect "A's EMBER" "$(balance "$ember" "${a[@]}")" 10000
expect "B's EMBER" "$(balance "$ember" "${b[@]}")" 300

say "a mint over the cap bounces and changes nothing"
"$dex" "${b[@]}" mint ASH 501
expect "B's EMBER" "$(balance "$ember" "${b[@]}")" 300

say "A deploys FLM/EMBER and deposits 100 FLM + 5000 EMBER"
"$dex" "${a[@]}" deploy-pool EMBER
"$dex" "${a[@]}" add EMBER 100 --amount 5000
"$dex" status EMBER ASH
expect "A's EMBER" "$(balance "$ember" "${a[@]}")" 5000

say "B sells 1 FLM for EMBER"
"$dex" "${b[@]}" swap EMBER flm 1
# 1e8 * 997 * 5000 / (100e8 * 1000 + 1e8 * 997) = 49
expect "B's EMBER" "$(balance "$ember" "${b[@]}")" 349

say "B sells 100 EMBER for FLM"
before="$(balance FLM "${b[@]}")"
"$dex" "${b[@]}" swap EMBER coin 100
after="$(balance FLM "${b[@]}")"
expect "B's EMBER" "$(balance "$ember" "${b[@]}")" 249
python3 -c "import sys; d=int(sys.argv[2])-int(sys.argv[1]); assert 190_000_000 < d < 200_000_000, d" "$before" "$after" \
    || { echo "FAIL: B's FLM moved by an unexpected amount" >&2; exit 1; }

say "a swap that misses its minimum pays B back"
before_flm="$(balance FLM "${b[@]}")"
"$dex" "${b[@]}" swap EMBER flm 1 --min-out 1000
expect "B's EMBER" "$(balance "$ember" "${b[@]}")" 249
expect "B's FLM" "$(balance FLM "${b[@]}")" "$before_flm"

say "A deposits 10 FLM more at the pool's price"
"$dex" "${a[@]}" add EMBER 10
"$dex" status EMBER ASH

say "A takes half of its first share back out"
"$dex" "${a[@]}" remove EMBER 5000000000
"$dex" status EMBER ASH

say "anyone renews a lease, topping up the fund"
"$dex" "${b[@]}" renew EMBER --pool --fund 5
"$dex" "${b[@]}" renew EMBER

"$dex" "${a[@]}" balance EMBER
"$dex" "${b[@]}" balance EMBER

say "a coin named by its actor id is checked against its state"
ash_id="$(field "['coins'][1]['actor']" <"$dir/coins.json")"
"$dex" status "$ash_id"
pool_id="$(field "['coins'][0]['pool']" <"$dir/coins.json")"
if "$dex" status "$pool_id" 2>"$dir/err"; then
    echo "FAIL: a pool passed for a coin" >&2; exit 1
fi
grep -q "not a flamedex coin" "$dir/err" || { cat "$dir/err" >&2; exit 1; }
say "PASS"
