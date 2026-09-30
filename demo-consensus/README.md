# Flame Consensus Demo

A local, interactive demo of Flame consensus anchored to Bitcoin. Explore both chains, create blocks and minters, send
acquisitions and votes, and watch how votes affect weights and branch selection.

The demo starts with Alice, a confirmed acquisition of 20,000 sats, and a genesis core block with an accepted vote and
weight 7. Subsequent votes are sent manually. Open **Instruction** in the top bar for a quick guide.

## Run

Requires Rust/Cargo and Node.js 22.12+ with npm on macOS or Linux. The first run needs internet access to download
dependencies and Bitcoin Core.

From the repository root:

```sh
node demo-consensus/dev.mjs
```

The launcher installs frontend dependencies if needed, builds the backend, and starts both services. Open *
*http://127.0.0.1:5173**; the API runs at **http://127.0.0.1:3001**.

Session data is temporary; restarting or clicking Reset creates a fresh demo.
