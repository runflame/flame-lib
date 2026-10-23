# Sourced by the devnet scripts beside it: builds flamed and flamewallet,
# starts a throwaway node with two-second blocks, points FLAMEWALLET_NODE
# at it, and defines the helpers the scripts check balances with. The node
# is stopped and its scratch directory removed on exit; on a failure the
# end of the node's log is printed first.
#
# Builds with -j1 by default; set CARGO_BUILD_JOBS to change that. Set
# KEEP=1 to keep the scratch directory and print its path. A script sets
# `port` before sourcing; PORT overrides it. It may also define
# `before_node`, run once the scratch directory exists, which can append
# lines to the node's flamed.toml through `node_toml` and register
# processes to stop on exit in `extra_pids`.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
port="${PORT:-$port}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"

cargo build --manifest-path "$root/Cargo.toml" -p flamed --features devnet
cargo build --manifest-path "$root/Cargo.toml" -p flamewallet-cli
flamed="$root/target/debug/flamed"
wallet="$root/target/debug/flamewallet"

dir="$(mktemp -d)"
node_pid=""
node_toml=""
extra_pids=()
cleanup() {
    local status=$?
    [[ -n "$node_pid" ]] && kill "$node_pid" 2>/dev/null && wait "$node_pid" 2>/dev/null || true
    for pid in "${extra_pids[@]}"; do
        kill "$pid" 2>/dev/null && wait "$pid" 2>/dev/null || true
    done
    if [[ $status != 0 && -f "$dir/node.log" ]]; then
        echo "--- last lines of the node's log" >&2
        tail -n 40 "$dir/node.log" >&2
    fi
    if [[ "${KEEP:-}" == 1 ]]; then echo "kept $dir"; else rm -rf "$dir"; fi
}
trap cleanup EXIT
if declare -F before_node >/dev/null; then before_node; fi

"$flamed" genesis --chainparams "$root/flamed/configs/chainparams.toml" --out "$dir/genesis.json"
cat >"$dir/flamed.toml" <<TOML
data_dir = "$dir/data"
genesis = "$dir/genesis.json"
rpc_bind = "127.0.0.1:$port"
block_interval_secs = 2
minimum_fee = 0
$node_toml
TOML
"$flamed" run --config "$dir/flamed.toml" >"$dir/node.log" 2>&1 &
node_pid=$!

export FLAMEWALLET_NODE="http://127.0.0.1:$port"
for _ in $(seq 100); do
    grep -q "serving JSON-RPC" "$dir/node.log" 2>/dev/null && break
    kill -0 "$node_pid" 2>/dev/null || { echo "FAIL: the node exited" >&2; exit 1; }
    sleep 0.2
done
grep -q "serving JSON-RPC" "$dir/node.log" || { echo "FAIL: the node never served" >&2; exit 1; }

# Native Flame's flavor, FLAME_FLAVOR = 1, as the wallet writes flavors.
flame="01$(printf '0%.0s' $(seq 62))"

field() { python3 -c "import json,sys; v=json.load(sys.stdin); print(eval('v' + sys.argv[1]))" "$1"; }
# The confirmed balance of one flavor (64 hex digits, or FLM), 0 if none.
balance() {
    local flavor="$1"; shift
    [[ "$flavor" == FLM ]] && flavor="$flame"
    "$wallet" "$@" --json balance | python3 -c "
import json, sys
balances = json.load(sys.stdin)['balances']
print(next((b['confirmed'] for b in balances if b['flavor'] == sys.argv[1]), '0'))" "$flavor"
}
# The confirmed amount of one flavor held at one path, 0 if none.
held_at() {
    local path="$1" flavor="$2"; shift 2
    [[ "$flavor" == FLM ]] && flavor="$flame"
    "$wallet" "$@" --json coins | python3 -c "
import json, sys
branch, index = map(int, sys.argv[1].split('/'))
print(sum(int(c['qty']) for c in json.load(sys.stdin)['coins']
          if (c['branch'], c['index']) == (branch, index) and c.get('flavor') == sys.argv[2]
          and c['height'] is not None))" "$path" "$flavor"
}
expect() { [[ "$2" == "$3" ]] || { echo "FAIL: $1: expected $3, got $2" >&2; exit 1; }; }
say() { printf '\n== %s\n' "$*"; }
show() { echo "-- $1"; "$wallet" "${@:2}" balance | sed 's/^/   /'; }
