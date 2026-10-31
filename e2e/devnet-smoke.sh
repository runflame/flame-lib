#!/usr/bin/env bash
# Restores the genesis wallet on a throwaway devnet, pays a fresh wallet
# with a memo, pays part of it back, pays a third wallet a cleartext token
# that it spends on, and checks every balance. Exits non-zero on the first
# thing that is wrong. Run from anywhere:
#
#   e2e/devnet-smoke.sh
#
# KEEP=1, PORT (default 18545) and CARGO_BUILD_JOBS as in devnet.sh.
port=18545
source "$(dirname "${BASH_SOURCE[0]}")/devnet.sh"

a=(--wallet "$dir/a.json")
b=(--wallet "$dir/b.json")
c=(--wallet "$dir/c.json")

# The genesis allocation is under the seed a1 x 64.
printf 'a1%.0s' $(seq 64) | "$wallet" "${a[@]}" restore --seed >/dev/null
"$wallet" "${b[@]}" new >/dev/null 2>&1
b_address="$("$wallet" "${b[@]}" --json address | field "[0]['address']")"
a_address="$("$wallet" "${a[@]}" --json address | field "[0]['address']")"

expect "A at genesis" "$(balance FLM "${a[@]}")" 1000000000000000000

"$wallet" "${a[@]}" send "$b_address" 12.5 --memo smoke --wait >/dev/null
expect "B after receiving" "$(balance FLM "${b[@]}")" 1250000000
expect "B's memo" "$("$wallet" "${b[@]}" --json history | field "['items'][0]['memo']")" smoke

"$wallet" "${b[@]}" send "$a_address" 2 --wait >/dev/null
expect "B after paying back" "$(balance FLM "${b[@]}")" 1049999000
expect "A after both" "$(balance FLM "${a[@]}")" 999999998949999000

# B's coins are confidential: a clear send opens only the payment.
"$wallet" "${c[@]}" new >/dev/null 2>&1
c_address="$("$wallet" "${c[@]}" --json address | field "[0]['address']")"
"$wallet" "${b[@]}" send "$c_address" 3 --clear --memo audit --wait >/dev/null
expect "C after a clear payment" "$(balance FLM "${c[@]}")" 300000000
expect "C's clear coin" "$(clear_held FLM "${c[@]}")" 300000000
expect "B's change stays confidential" "$(clear_held FLM "${b[@]}")" 0
expect "B after paying clear" "$(balance FLM "${b[@]}")" 749998000

# A cleartext coin spends like any other.
"$wallet" "${c[@]}" send "$b_address" 1 --wait >/dev/null
expect "C after spending it" "$(balance FLM "${c[@]}")" 199999000
expect "C's change is confidential" "$(clear_held FLM "${c[@]}")" 0
expect "B after all" "$(balance FLM "${b[@]}")" 849998000

echo "devnet smoke: ok"
