#!/usr/bin/env bash
# Checks that the wallet's history names the other side of every
# transaction. The genesis wallet A pays a fresh wallet B, and B pays part
# of it back; each side's history, pending and confirmed, names the other
# by tracking address: `from` who paid the wallet, `to` whom it paid.
# Both sides also show the fee the payer paid.
# Exits non-zero on the first thing that is wrong. Run from anywhere:
#
#   e2e/devnet-counterparty.sh
#
# KEEP=1, PORT (default 18547) and CARGO_BUILD_JOBS as in devnet.sh.
port=18547
# The pending checks run between two blocks: this is their time.
block_interval=10
source "$(dirname "${BASH_SOURCE[0]}")/devnet.sh"

a=(--wallet "$dir/a.json")
b=(--wallet "$dir/b.json")

# One field of a wallet's newest history item: `newest from "${b[@]}"`.
# A list comes back sorted and space-separated, since a transaction's
# outputs are in no promised order.
newest() {
    local key="$1"; shift
    "$wallet" "$@" --json history | python3 -c "
import json, sys
v = json.load(sys.stdin)['items'][0][sys.argv[1]]
print(' '.join(sorted(v)) if isinstance(v, list) else v)" "$key"
}
# A tracking address is a receiving address's first half.
tracking() { echo "${1:0:60}"; }

# The genesis allocation is under the seed a1 x 64.
printf 'a1%.0s' $(seq 64) | "$wallet" "${a[@]}" restore --seed >/dev/null
"$wallet" "${b[@]}" new >/dev/null 2>&1
a_address="$("$wallet" "${a[@]}" --json address | field "[0]['address']")"
b_address="$("$wallet" "${b[@]}" --json address | field "[0]['address']")"

say "Genesis: A's allocation came from no one"
expect "A's allocation, from" "$(newest from "${a[@]}")" ""
expect "A's allocation, to" "$(newest to "${a[@]}")" ""
expect "A's allocation paid no fee" "$(newest fee_sparks "${a[@]}")" 0

say "A pays B 12.5 Flame; pending, both sides name the other"
next_block
"$wallet" "${a[@]}" send "$b_address" 12.5 --memo "from A" >/dev/null
expect "B's item is pending" "$(newest pending "${b[@]}")" True
expect "B was paid from A" "$(newest from "${b[@]}")" "$(tracking "$a_address")"
expect "B paid no one" "$(newest to "${b[@]}")" ""
expect "B's memo" "$(newest memo "${b[@]}")" "from A"
expect "A paid B" "$(newest to "${a[@]}")" "$(tracking "$b_address")"
expect "A was paid by no one" "$(newest from "${a[@]}")" ""

say "Confirmed, they say the same"
"$wallet" "${a[@]}" wait "$(newest txid "${a[@]}")" >/dev/null
expect "B's item is confirmed" "$(newest pending "${b[@]}")" False
expect "B was paid from A" "$(newest from "${b[@]}")" "$(tracking "$a_address")"
expect "A paid B" "$(newest to "${a[@]}")" "$(tracking "$b_address")"

say "B pays 2 back, with a fee of 2500 sparks"
"$wallet" "${b[@]}" send "$a_address" 2 --fee 2500 --wait >/dev/null
expect "A was paid from B" "$(newest from "${a[@]}")" "$(tracking "$b_address")"
expect "A paid no one" "$(newest to "${a[@]}")" ""
expect "B paid A" "$(newest to "${b[@]}")" "$(tracking "$a_address")"
expect "B was paid by no one" "$(newest from "${b[@]}")" ""
expect "B's fee" "$(newest fee_sparks "${b[@]}")" 2500
expect "A sees B's fee" "$(newest fee_sparks "${a[@]}")" 2500

say "As B's history prints it"
"$wallet" "${b[@]}" history | sed 's/^/   /'
"$wallet" "${b[@]}" history | grep -q "from $(tracking "$a_address")" ||
    { echo "FAIL: B's history does not print who paid it" >&2; exit 1; }
[[ "$("$wallet" "${b[@]}" history | sed -n 1p)" == *"  fee 0.000025 FLM"* ]] ||
    { echo "FAIL: B's history does not print the fee it paid" >&2; exit 1; }

echo
echo "devnet counterparty: ok"
