#!/usr/bin/env bash
# Starts a throwaway devnet node and runs the whole DEX on it: the genesis
# wallet A deploys two coins and the FLM/EMBER pool, mints, provides
# liquidity; a fresh wallet B mints, swaps both ways and is refunded when
# its price limit is missed; A takes liquidity out and renews a lease.
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

say "A pays B 50 Flame"
"$wallet" "${a[@]}" send "$b_address" 50 --wait

say "A deploys the coins EMBER and ASH"
"$dex" "${a[@]}" deploy-coin EMBER
"$dex" "${a[@]}" deploy-coin ASH --max 1000 --cap 500
# No registry: both are named by what they were built from, ASH with its limits.
ember="$("$dex" describe EMBER | field "['coins'][0]['flavor']")"

say "deploying EMBER again is refused before anything is sent"
if "$dex" "${a[@]}" deploy-coin EMBER 2>"$dir/err"; then
    echo "FAIL: a second EMBER deployed" >&2; exit 1
fi
grep -q "already deployed" "$dir/err" || { cat "$dir/err" >&2; exit 1; }

say "A mints 10000 EMBER, B mints 300"
"$dex" "${a[@]}" mint EMBER 10000
"$dex" "${b[@]}" mint EMBER 300
expect "A's EMBER" "$(balance "$ember" "${a[@]}")" 10000
expect "B's EMBER" "$(balance "$ember" "${b[@]}")" 300

say "a mint over the cap bounces and changes nothing"
"$dex" "${b[@]}" mint ASH:1000:500 501
expect "B's EMBER" "$(balance "$ember" "${b[@]}")" 300

say "A deploys FLM/EMBER and deposits 100 FLM + 5000 EMBER"
"$dex" "${a[@]}" deploy-pool EMBER
"$dex" "${a[@]}" add EMBER 100 --amount 5000
"$dex" status EMBER ASH:1000:500
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
"$dex" status EMBER ASH:1000:500

say "A takes half of its first share back out"
"$dex" "${a[@]}" remove EMBER 5000000000
"$dex" status EMBER ASH:1000:500

say "anyone renews a lease, topping up the fund"
"$dex" "${b[@]}" renew EMBER --pool --fund 5
"$dex" "${b[@]}" renew EMBER

"$dex" "${a[@]}" balance EMBER
"$dex" "${b[@]}" balance EMBER

say "a coin named by its actor id is checked against its state"
ash_id="$("$dex" describe ASH:1000:500 | field "['coins'][0]['actor']")"
"$dex" status "$ash_id"
pool_id="$("$dex" describe EMBER | field "['pools'][0]['actor']")"
if "$dex" status "$pool_id" 2>"$dir/err"; then
    echo "FAIL: a pool passed for a coin" >&2; exit 1
fi
grep -q "not a flamedex coin" "$dir/err" || { cat "$dir/err" >&2; exit 1; }
say "PASS"
