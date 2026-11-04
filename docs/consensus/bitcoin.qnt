// -*- mode: Bluespec; -*-
/**
 * Bitcoin as seen by a protocol that reads it: indexers, wallets, sidechains.
 *
 * The module knows nothing about Flame. Output owners and tags are opaque strings;
 * any protocol meaning of a transaction is kept by the importing model in its own
 * table (txid -> payload).
 *
 * Part 1 is pure: types, queries and transitions Btc -> Btc.
 * Part 2 is the network as a component: state, actions and the environment assumptions.
 * A protocol model imports the whole module as an instance and composes it:
 *   import bitcoin(OBSERVERS = ..., ...) as B from "./bitcoin"
 *   action step = any { all { B::envStep, myVars' = myVars }, myActions }   // myActions use B::relayTx(tx)
 *   val bitcoinStillOk = B::allInvariants
 *
 * Abstractions (deliberate):
 *  - work = height: every block carries one unit of work, so "most work" is "highest";
 *  - no PoW, difficulty, time/MTP, fee market, block size, scripts: an output has an
 *    owner and a `spendable` flag (false = OP_RETURN or burn);
 *  - no signatures: `owner` is a label, nothing checks that a spender owns the input.
 *    Who may spend what is the importer's business: its actions choose what goes into
 *    relayTx. The standalone `step` lets anyone spend anything;
 *  - a block body is a set: inputs refer only to earlier transactions (`shapeOk`), so
 *    ascending TxId is a valid order inside a block, and every valid order gives the same
 *    UTXO set and inclusion heights. An importer whose result depends on the order of
 *    transactions inside one block cannot take that order from this module;
 *  - the mempool is global: every transaction ever relayed. What a miner may include
 *    is decided per branch (`validBody`), so after a reorg orphaned transactions are
 *    eligible again without any "return to mempool" step, and transactions that
 *    conflict with the new branch drop out by themselves;
 *  - conflicting relayed transactions (RBF, double-spend attempts) coexist in the
 *    global mempool; a miner picks either, which over-approximates every RBF policy;
 *  - the adversary may sign a transaction and keep it (`withholdTx`): only its own
 *    blocks include it until it relays it after all (`revealTx`). Publishing a block
 *    reveals its transactions to every node, so an orphaned one returns to the mempool.
 *    Nodes drop orphans: a relayed transaction spends only outputs of transactions they
 *    know;
 *  - coinbase transactions exist only in genesis: they fund the owners.
 *
 * Contract with an importing model.
 *
 * Guaranteed in every reachable state for any K_REORG and CENSOR_WINDOW
 * (`consensusInvariants`): the block tree is well formed, every body is valid, no
 * transaction twice on a branch, no inflation, withheld blocks only above published ones,
 * private transactions only in withheld blocks, observer views and honest blocks are
 * published, the indexer stream between any two blocks lands on the target branch.
 *
 * Reachable (bitcoin_test.qnt, H): reorgs of K_REORG blocks, conflicting transactions on
 * two branches, a transaction re-included at another height, observers on different tips
 * (also at one height), a mined transaction back in the mempool, an owner holding a coin
 * with nothing to spend, honest forks, a private double spend.
 *
 * Assumed, not guaranteed by Bitcoin (`assumptionInvariants`, parameters in Part 2).
 * A property checked with them holds only under them; K_REORG = CENSOR_WINDOW =
 * MAX_BLOCKS switches them off:
 *  - K_REORG restricts which branches may exist at all, not only the reorgs that happen:
 *    no block is published (honest ones are, at once) on a fork deeper than K_REORG
 *    below the best tip, so no observer ever reorganizes deeper. This excludes valid
 *    Bitcoin runs, e.g. a stale branch published late and adopted by a lagging node;
 *  - CENSOR_WINDOW: a transaction known to every node waits at most CENSOR_WINDOW honest
 *    blocks.
 *
 * Real runs the model excludes, so a property may hold here only because the run that
 * breaks it is unreachable:
 *  - stale branches forking deeper than K_REORG below the best tip, even if nobody
 *    adopts them;
 *  - per-node block sets: a published block reaches every node at once, only adopting it
 *    is delayed (`observe`). A local bitcoind never knows a block that others do not, so
 *    runs with eclipsed, partitioned or lagging nodes are covered only as far as a
 *    delayed `observe` covers them;
 *  - transaction order inside a block, and everything listed under Abstractions.
 */
module bitcoin {


  // ------------------------------------------------------------------
  // Types
  // ------------------------------------------------------------------

  type Hash = int
  type TxId = int
  type Outpoint = (TxId, int)                       // (txid, vout)
  type TxOut = { owner: str, value: int, spendable: bool }
  type Tx = { id: TxId, inputs: Set[Outpoint], outputs: List[TxOut] }
  type Block = {
    hash: Hash,
    parent: Hash,
    height: int,
    txs: Set[TxId],                                 // unordered: see the header
    published: bool,                                // false = withheld (private fork)
    seen: int,                                      // publication order, -1 while withheld
    miner: str,                                     // opaque label, e.g. "honest"
  }
  type Btc = {
    blocks: Hash -> Block,
    txs: TxId -> Tx,                                // every transaction ever signed
    broadcast: Set[TxId],                           // known to every node: relayed or revealed
                                                    // by a published block (= global mempool
                                                    // before filtering); the rest is private
    nextHash: Hash,
    nextSeen: int,
  }

  pure val GENESIS: Hash = 0
  pure val GENESIS_TX: TxId = 0
  pure val NO_BLOCK: Hash = -1

  // ------------------------------------------------------------------
  // Constructors (new fields get their defaults here, not in every test)
  // ------------------------------------------------------------------

  pure def mkOut(owner: str, value: int): TxOut = { owner: owner, value: value, spendable: true }

  // OP_RETURN (value 0) or a burn (value > 0): can never be spent
  pure def mkUnspendable(tag: str, value: int): TxOut = { owner: tag, value: value, spendable: false }

  pure def mkTx(id: TxId, inputs: Set[Outpoint], outputs: List[TxOut]): Tx =
    { id: id, inputs: inputs, outputs: outputs }

  pure def genesis(funding: List[TxOut]): Btc = {
    blocks: Map(GENESIS -> {
      hash: GENESIS, parent: NO_BLOCK, height: 0, txs: Set(GENESIS_TX), published: true, seen: 0, miner: "genesis",
    }),
    txs: Map(GENESIS_TX -> mkTx(GENESIS_TX, Set(), funding)),
    broadcast: Set(GENESIS_TX),
    nextHash: 1,
    nextSeen: 1,
  }

  // ------------------------------------------------------------------
  // Transitions
  // ------------------------------------------------------------------

  // relay a transaction (the caller checks `wellFormed` and a fresh id)
  pure def withTx(c: Btc, tx: Tx): Btc =
    { ...c, txs: c.txs.put(tx.id, tx), broadcast: c.broadcast.union(Set(tx.id)) }

  // the adversary signs a transaction and tells nobody (the caller checks `wellFormed`)
  pure def withPrivateTx(c: Btc, tx: Tx): Btc = { ...c, txs: c.txs.put(tx.id, tx) }

  // the adversary relays a private transaction after all (the caller checks `revealable`)
  pure def withRevealedTx(c: Btc, id: TxId): Btc = { ...c, broadcast: c.broadcast.union(Set(id)) }

  // a new block gets hash `c.nextHash` (the caller checks `validBody`)
  pure def withBlock(c: Btc, parent: Hash, ids: Set[TxId], published: bool, miner: str): Btc = {
    ...c,
    blocks: c.blocks.put(c.nextHash, {
      hash: c.nextHash, parent: parent, height: c.blocks.get(parent).height + 1,
      txs: ids, published: published, seen: if (published) c.nextSeen else -1, miner: miner,
    }),
    broadcast: if (published) c.broadcast.union(ids) else c.broadcast,
    nextHash: c.nextHash + 1,
    nextSeen: if (published) c.nextSeen + 1 else c.nextSeen,
  }

  // relaying a block relays its ancestors too, and reveals their transactions. They share
  // one `seen`: their heights differ, so it never breaks a tie between them
  pure def publishBranch(c: Btc, h: Hash): Btc = {
    val fresh = branch(c, h).filter(b => not(c.blocks.get(b).published))
    {
      ...c,
      blocks: fresh.fold(c.blocks, (bs, b) => bs.set(b, { ...bs.get(b), published: true, seen: c.nextSeen })),
      broadcast: c.broadcast.union(txsOf(c, fresh)),
      nextSeen: c.nextSeen + 1,
    }
  }

  // ------------------------------------------------------------------
  // Tree and branches
  // ------------------------------------------------------------------

  pure def heightOf(c: Btc, h: Hash): int = c.blocks.get(h).height

  // [tip, parent, ..., genesis]
  pure def descChain(c: Btc, tip: Hash): List[Hash] =
    range(0, heightOf(c, tip)).foldl([tip], (acc, _i) => acc.append(c.blocks.get(acc[acc.length() - 1]).parent))

  // chain(c, tip)[k] = the block at height k on the branch ending at tip
  pure def chain(c: Btc, tip: Hash): List[Hash] = {
    val d = descChain(c, tip)
    range(0, d.length()).foldl([], (acc, i) => acc.append(d[d.length() - 1 - i]))
  }

  pure def branch(c: Btc, tip: Hash): Set[Hash] =
    descChain(c, tip).foldl(Set(), (s, h) => s.union(Set(h)))

  pure def isAncestor(c: Btc, a: Hash, b: Hash): bool = branch(c, b).contains(a)

  // highest common block of two branches
  pure def ancestor(c: Btc, a: Hash, b: Hash): Hash = {
    val ca = chain(c, a)
    val cb = chain(c, b)
    val n = if (ca.length() < cb.length()) ca.length() else cb.length()
    range(0, n).foldl(GENESIS, (r, k) => if (ca[k] == cb[k]) ca[k] else r)
  }

  // ------------------------------------------------------------------
  // Transactions on a branch
  // ------------------------------------------------------------------

  pure def txsOf(c: Btc, hs: Set[Hash]): Set[TxId] = hs.fold(Set(), (s, h) => s.union(c.blocks.get(h).txs))

  pure def onBranch(c: Btc, tip: Hash): Set[TxId] = txsOf(c, branch(c, tip))

  pure def inclBlock(c: Btc, tip: Hash, id: TxId): Hash =
    branch(c, tip).fold(NO_BLOCK, (r, h) => if (c.blocks.get(h).txs.contains(id)) h else r)

  // -1 if the transaction is not on the branch
  pure def inclHeight(c: Btc, tip: Hash, id: TxId): int = {
    val b = inclBlock(c, tip, id)
    if (b == NO_BLOCK) -1 else heightOf(c, b)
  }

  pure def confirmations(c: Btc, tip: Hash, id: TxId): int = {
    val k = inclHeight(c, tip, id)
    if (k < 0) 0 else heightOf(c, tip) - k + 1
  }

  // ------------------------------------------------------------------
  // Outputs, values, conflicts
  // ------------------------------------------------------------------

  pure def isOutpoint(c: Btc, op: Outpoint): bool =
    if (c.txs.keys().contains(op._1)) op._2 >= 0 and op._2 < c.txs.get(op._1).outputs.length() else false

  pure def outAt(c: Btc, op: Outpoint): TxOut = c.txs.get(op._1).outputs[op._2]

  pure def outputsOf(c: Btc, id: TxId): Set[Outpoint] = c.txs.get(id).outputs.indices().map(i => (id, i))

  pure def created(c: Btc, ids: Set[TxId]): Set[Outpoint] = ids.fold(Set(), (s, id) => s.union(outputsOf(c, id)))

  pure def spendableOuts(c: Btc, ids: Set[TxId]): Set[Outpoint] =
    created(c, ids).filter(op => outAt(c, op).spendable)

  pure def spent(c: Btc, ids: Set[TxId]): Set[Outpoint] = ids.fold(Set(), (s, id) => s.union(c.txs.get(id).inputs))

  pure def valueIn(c: Btc, tx: Tx): int =
    tx.inputs.fold(0, (s, op) => if (isOutpoint(c, op)) s + outAt(c, op).value else s)

  pure def valueOut(tx: Tx): int = tx.outputs.foldl(0, (s, o) => s + o.value)

  pure def conflicts(c: Btc, a: TxId, b: TxId): bool =
    a != b and c.txs.get(a).inputs.intersect(c.txs.get(b).inputs) != Set()

  pure def utxoSet(c: Btc, tip: Hash): Set[Outpoint] = {
    val ids = onBranch(c, tip)
    spendableOuts(c, ids).exclude(spent(c, ids))
  }

  // CheckTransaction for every transaction but the genesis coinbase. Inputs only from
  // earlier transactions: a txid commits to its inputs, so spends never form a cycle
  pure def shapeOk(tx: Tx): bool = and {
    tx.inputs != Set(),                                              // no new coinbase
    tx.outputs != [],
    tx.outputs.indices().forall(i => tx.outputs[i].value >= 0),
    tx.inputs.forall(op => op._1 < tx.id),                           // no self-spend, no cycle
  }

  // spends an output of a transaction not every node knows: nodes drop it
  pure def orphan(c: Btc, tx: Tx): bool = tx.inputs.exists(op => not(c.broadcast.contains(op._1)))

  // a private transaction that nodes would accept now
  pure def revealable(c: Btc, id: TxId): bool = and {
    c.txs.keys().contains(id),
    not(c.broadcast.contains(id)),
    not(orphan(c, c.txs.get(id))),
  }

  // relay rule: spends existing spendable outputs (anywhere in the tree), no inflation
  pure def wellFormed(c: Btc, tx: Tx): bool = and {
    shapeOk(tx),
    tx.inputs.forall(op => if (isOutpoint(c, op)) outAt(c, op).spendable else false),
    valueIn(c, tx) >= valueOut(tx),
  }

  // consensus rule for a block body on top of `parent`
  pure def validBody(c: Btc, parent: Hash, ids: Set[TxId]): bool =
    if (not(ids.subseteq(c.txs.keys()))) false
    else {
      val prior = onBranch(c, parent)
      val avail = spendableOuts(c, prior.union(ids)).exclude(spent(c, prior))
      and {
        ids.intersect(prior) == Set(),                                   // no double inclusion (also implied
                                                                         // by the UTXO rule: every tx has inputs)
        ids.forall(id => shapeOk(c.txs.get(id))),
        ids.forall(id => c.txs.get(id).inputs.subseteq(avail)),          // inputs exist, unspent
        ids.forall(a => ids.forall(b => not(conflicts(c, a, b)))),        // no double spend inside
        ids.forall(id => valueIn(c, c.txs.get(id)) >= valueOut(c.txs.get(id))),
      }
    }

  // ------------------------------------------------------------------
  // Mempool relative to a tip
  // ------------------------------------------------------------------

  // what a miner on `tip` may include from `pool`: not on the branch, not conflicting with
  // it, and every input either unspent on the branch or created by another eligible
  // transaction (packages)
  pure def eligible(c: Btc, tip: Hash, pool: Set[TxId]): Set[TxId] = {
    val prior = onBranch(c, tip)
    val spentPrior = spent(c, prior)
    val base = spendableOuts(c, prior).exclude(spentPrior)
    val cand0 = pool.exclude(prior).filter(id => c.txs.get(id).inputs.intersect(spentPrior) == Set())
    range(0, cand0.size()).foldl(cand0, (cand, _i) =>
      cand.filter(id => c.txs.get(id).inputs.subseteq(base.union(spendableOuts(c, cand)))))
  }

  // what every node knows and any miner may include on top of `tip`
  pure def mempool(c: Btc, tip: Hash): Set[TxId] = eligible(c, tip, c.broadcast)

  // ------------------------------------------------------------------
  // Wallet: Bitcoin Core `listunspent minconf` for one owner (minConf >= 1)
  // ------------------------------------------------------------------

  // confirmed outputs of the owner, minus those spent by a pending transaction the wallet
  // knows of. `known`: what reached the wallet (its own transactions, its node's mempool);
  // `c.broadcast` is a wallet that hears of everything at once. With less it may return an
  // output that an unseen transaction already spends: spending it again is a conflict
  pure def listUnspent(c: Btc, tip: Hash, owner: str, known: Set[TxId], minConf: int): Set[Outpoint] = {
    val pendingSpent = spent(c, mempool(c, tip).intersect(known))
    utxoSet(c, tip).filter(op => and {
      outAt(c, op).owner == owner,
      confirmations(c, tip, op._1) >= minConf,
      not(pendingSpent.contains(op)),
    })
  }

  // ------------------------------------------------------------------
  // Fork choice and observers
  // ------------------------------------------------------------------

  pure def publishedBlocks(c: Btc): Set[Hash] = c.blocks.keys().filter(h => c.blocks.get(h).published)

  // an observer (node, wallet, proposer) switches only to a published block with strictly
  // more work: among equal-work blocks it keeps the one it saw first
  pure def adopts(c: Btc, current: Hash, candidate: Hash): bool =
    c.blocks.get(candidate).published and heightOf(c, candidate) > heightOf(c, current)

  // network-wide best tip: the view of a node that receives every block the moment it is
  // published. Most work, ties to the block published first, as in `adopts`
  pure def bestTip(c: Btc): Hash =
    publishedBlocks(c).fold(GENESIS, (b, h) => {
      val x = c.blocks.get(h)
      val y = c.blocks.get(b)
      if (x.height > y.height or (x.height == y.height and x.seen < y.seen)) h else b
    })

  pure def reorgDepth(c: Btc, src: Hash, dst: Hash): int = heightOf(c, src) - heightOf(c, ancestor(c, src, dst))

  // ------------------------------------------------------------------
  // Indexer event stream (as BitcoinChainUpdatePlanner in flame-lib)
  // ------------------------------------------------------------------

  // blocks to disconnect, tip first
  pure def disconnects(c: Btc, cursor: Hash, target: Hash): List[Hash] =
    descChain(c, cursor).slice(0, reorgDepth(c, cursor, target))

  // blocks to connect, lowest first; a page of the planner is a prefix
  pure def connects(c: Btc, cursor: Hash, target: Hash): List[Hash] = {
    val ch = chain(c, target)
    ch.slice(heightOf(c, ancestor(c, cursor, target)) + 1, ch.length())
  }

  // what an indexer holds: its cursor and the inclusion height of every indexed transaction.
  // `ok` turns false on an event that does not fit the cursor
  type Index = { ok: bool, cursor: Hash, incl: TxId -> int }

  pure def indexAt(c: Btc, tip: Hash): Index =
    { ok: true, cursor: tip, incl: onBranch(c, tip).mapBy(id => inclHeight(c, tip, id)) }

  // only the cursor block can be disconnected
  pure def disconnectBlock(c: Btc, s: Index, h: Hash): Index = {
    ok: s.ok and h == s.cursor and h != GENESIS,
    cursor: c.blocks.get(h).parent,
    incl: s.incl.keys().exclude(c.blocks.get(h).txs).mapBy(id => s.incl.get(id)),
  }

  // only a child of the cursor can be connected; its transactions get its height
  pure def connectBlock(c: Btc, s: Index, h: Hash): Index = {
    ok: s.ok and c.blocks.get(h).parent == s.cursor,
    cursor: h,
    incl: c.blocks.get(h).txs.fold(s.incl, (m, id) => m.put(id, heightOf(c, h))),
  }

  pure def applyStream(c: Btc, s: Index, disc: List[Hash], conn: List[Hash]): Index =
    conn.foldl(disc.foldl(s, (acc, h) => disconnectBlock(c, acc, h)), (acc, h) => connectBlock(c, acc, h))

  pure def replay(c: Btc, cursor: Hash, target: Hash): Index =
    applyStream(c, indexAt(c, cursor), disconnects(c, cursor, target), connects(c, cursor, target))

  // the stream, cut after any number of connects (a page), leaves the indexer as if it had
  // followed the target branch: cursor, transactions, their heights (so confirmations)
  pure def streamOk(c: Btc, cursor: Hash, target: Hash): bool = {
    val afterDisc = applyStream(c, indexAt(c, cursor), disconnects(c, cursor, target), [])
    val conn = connects(c, cursor, target)
    val base = heightOf(c, ancestor(c, cursor, target))
    0.to(conn.length()).forall(k =>
      applyStream(c, afterDisc, [], conn.slice(0, k)) == indexAt(c, chain(c, target)[base + k]))
  }

  // ------------------------------------------------------------------
  // Well-formedness of a whole Btc value (reusable as invariants by importers)
  // ------------------------------------------------------------------

  pure def treeWellFormed(c: Btc): bool = and {
    c.blocks.keys().contains(GENESIS),
    heightOf(c, GENESIS) == 0,
    c.blocks.keys().forall(h => c.blocks.get(h).hash == h),
    c.blocks.keys().forall(h => h == GENESIS or and {
      c.blocks.keys().contains(c.blocks.get(h).parent),
      c.blocks.get(h).parent < h,                                       // parents are created first
      heightOf(c, h) == heightOf(c, c.blocks.get(h).parent) + 1,
    }),
    c.blocks.keys().forall(h => h < c.nextHash),
    c.blocks.keys().forall(h => c.blocks.get(h).published == (c.blocks.get(h).seen >= 0)),
    c.blocks.keys().forall(h => c.blocks.get(h).seen < c.nextSeen),
    publishedBlocks(c).forall(a => publishedBlocks(c).forall(b =>                     // ties are ordered
      a == b or heightOf(c, a) != heightOf(c, b) or c.blocks.get(a).seen != c.blocks.get(b).seen)),
  }

  pure def allBodiesValid(c: Btc): bool =
    c.blocks.keys().forall(h => h == GENESIS or validBody(c, c.blocks.get(h).parent, c.blocks.get(h).txs))

  pure def noTxTwiceOnBranch(c: Btc): bool =
    c.blocks.keys().forall(tip => {
      val bs = branch(c, tip)
      bs.forall(x => bs.forall(y => x == y or c.blocks.get(x).txs.intersect(c.blocks.get(y).txs) == Set()))
    })

  pure def valueConserved(c: Btc): bool =
    c.txs.keys().forall(id => id == GENESIS_TX or valueIn(c, c.txs.get(id)) >= valueOut(c.txs.get(id)))

  pure def withheldOnlyAbovePublished(c: Btc): bool =
    publishedBlocks(c).forall(h => branch(c, h).subseteq(publishedBlocks(c)))

  // a private transaction sits only in withheld blocks
  pure def publishedRevealed(c: Btc): bool = txsOf(c, publishedBlocks(c)).subseteq(c.broadcast)

  // ==================================================================
  // Part 2. The network as a component
  // ==================================================================

  /*
   * Bitcoin's own state machine: relaying, honest and adversarial mining, publishing,
   * observers following the chain. Used to check the library in bitcoin.qnt as a system;
   * a protocol model imports bitcoin.qnt and wraps the same transitions in its actions.
   *
   * Honest miners build on any published block within K_REORG of the best tip: a node
   * with a stale view, or one that saw the other side of a tie first.
   *
   * Environment assumptions (parameters). Bitcoin guarantees neither: they are what a
   * protocol assumes, and a property checked with them holds only under them (see the
   * contract in the header). K_REORG = CENSOR_WINDOW = MAX_BLOCKS switches both off.
   *  - K_REORG: no block, honest or not, is published on a fork deeper than K_REORG
   *    below the best tip, and the adversary mines only on blocks at most K_REORG below
   *    it. A limit on which branches exist, so also on every observer's reorgs;
   *  - CENSOR_WINDOW: an honest miner includes every transaction that has waited
   *    CENSOR_WINDOW honest blocks, unless it conflicts with what the block includes.
   */


  const OBSERVERS: Set[str]
  const OWNERS: Set[str]
  const K_REORG: int
  const CENSOR_WINDOW: int
  const MAX_BLOCKS: int
  const MAX_TXS: int

  var btc: Btc
  var views: str -> Hash             // each observer's current tip
  var maxReorg: int                  // deepest reorg any observer went through
  var bcastHeight: TxId -> int       // best height when every node learned of the tx

  pure val HONEST = "honest"
  pure val ADVERSARY = "adversary"

  // honest blocks on the branch mined after the transaction was relayed
  pure def honestAge(c: Btc, tip: Hash, since: int): int =
    branch(c, tip).filter(h => c.blocks.get(h).miner == HONEST and heightOf(c, h) > since).size()

  pure def allOutpoints(c: Btc): Set[Outpoint] = spendableOuts(c, c.txs.keys())

  // ------------------------------------------------------------------
  // Actions
  // ------------------------------------------------------------------

  action init = all {
    btc' = genesis(OWNERS.fold([], (l, o) => l.append(mkOut(o, 10)))),
    views' = OBSERVERS.mapBy(_o => GENESIS),
    maxReorg' = 0,
    bcastHeight' = Map(),
  }

  // relay a protocol transaction: with revealTx and publish the only ways to make a tx
  // known, so bcastHeight stays right
  action relayTx(tx: Tx): bool = all {
    tx.id == btc.txs.keys().size(),
    tx.id <= MAX_TXS,
    wellFormed(btc, tx),
    not(orphan(btc, tx)),
    btc' = withTx(btc, tx),
    bcastHeight' = bcastHeight.put(tx.id, heightOf(btc, bestTip(btc))),
    views' = views,
    maxReorg' = maxReorg,
  }

  // the adversary signs a transaction and keeps it for its own blocks
  action withholdTx(tx: Tx): bool = all {
    tx.id == btc.txs.keys().size(),
    tx.id <= MAX_TXS,
    wellFormed(btc, tx),
    btc' = withPrivateTx(btc, tx),
    bcastHeight' = bcastHeight,
    views' = views,
    maxReorg' = maxReorg,
  }

  // the adversary relays one of its private transactions: it starts waiting now
  action revealTx(id: TxId): bool = all {
    revealable(btc, id),
    btc' = withRevealedTx(btc, id),
    bcastHeight' = bcastHeight.put(id, heightOf(btc, bestTip(btc))),
    views' = views,
    maxReorg' = maxReorg,
  }

  // an owner spends any existing output (possibly already spent somewhere: a conflict)
  // into [data, value to `payee`], like a protocol transaction with change
  pure def payment(c: Btc, op: Outpoint, payee: str): Tx =
    mkTx(c.txs.keys().size(), Set(op), [mkUnspendable("data", 0), mkOut(payee, outAt(c, op).value)])

  action relay(op: Outpoint, payee: str): bool = relayTx(payment(btc, op, payee))

  action withhold(op: Outpoint, payee: str): bool = withholdTx(payment(btc, op, payee))

  // published at once; the block on `parent` reorganizes the best chain as deep as `parent`
  action mineHonest(parent: Hash, ids: Set[TxId]): bool = {
    val aged = mempool(btc, parent).filter(t => honestAge(btc, parent, bcastHeight.get(t)) >= CENSOR_WINDOW)
    all {
      btc.nextHash < MAX_BLOCKS,
      btc.blocks.get(parent).published,
      reorgDepth(btc, bestTip(btc), parent) <= K_REORG,
      ids.subseteq(mempool(btc, parent)),
      validBody(btc, parent, ids),
      aged.forall(t => ids.contains(t) or not(validBody(btc, parent, ids.union(Set(t))))),
      btc' = withBlock(btc, parent, ids, true, HONEST),
      views' = views,
      maxReorg' = maxReorg,
      bcastHeight' = bcastHeight,
    }
  }

  // private mining on any recent block, any valid subset (censorship) of what the
  // adversary knows: the mempool and its private transactions; not relayed yet
  action mineAdversary(parent: Hash, ids: Set[TxId]): bool = all {
    btc.nextHash < MAX_BLOCKS,
    heightOf(btc, parent) >= heightOf(btc, bestTip(btc)) - K_REORG,
    ids.subseteq(eligible(btc, parent, btc.txs.keys())),
    validBody(btc, parent, ids),
    btc' = withBlock(btc, parent, ids, false, ADVERSARY),
    views' = views,
    maxReorg' = maxReorg,
    bcastHeight' = bcastHeight,
  }

  // a revealed private transaction starts waiting for honest miners now
  action publish(h: Hash): bool = {
    val c = publishBranch(btc, h)
    all {
      not(btc.blocks.get(h).published),
      reorgDepth(btc, bestTip(btc), h) <= K_REORG,
      btc' = c,
      views' = views,
      maxReorg' = maxReorg,
      bcastHeight' = c.broadcast.exclude(btc.broadcast).fold(bcastHeight, (m, t) => m.put(t, heightOf(c, bestTip(c)))),
    }
  }

  action observe(o: str, h: Hash): bool = {
    val d = reorgDepth(btc, views.get(o), h)
    all {
      adopts(btc, views.get(o), h),
      views' = views.set(o, h),
      maxReorg' = if (d > maxReorg) d else maxReorg,
      btc' = btc,
      bcastHeight' = bcastHeight,
    }
  }

  // what the network does on its own: mining, withholding, publishing, observers moving
  action envStep = any {
    nondet parent = publishedBlocks(btc).oneOf()
    nondet ids = mempool(btc, parent).powerset().oneOf()
    mineHonest(parent, ids),
    nondet parent = btc.blocks.keys().oneOf()
    nondet ids = eligible(btc, parent, btc.txs.keys()).powerset().oneOf()
    mineAdversary(parent, ids),
    nondet h = btc.blocks.keys().oneOf()
    publish(h),
    nondet id = btc.txs.keys().oneOf()
    revealTx(id),
    nondet o = OBSERVERS.oneOf()
    nondet h = btc.blocks.keys().oneOf()
    observe(o, h),
  }

  // standalone Bitcoin: the network plus arbitrary payments, public or private
  action step = any {
    envStep,
    nondet op = allOutpoints(btc).oneOf()
    nondet payee = OWNERS.oneOf()
    any { relay(op, payee), withhold(op, payee) },
  }

  // ------------------------------------------------------------------
  // G. Invariants: must hold in every reachable state
  // ------------------------------------------------------------------

  // consensus: follow from the rules, for any K_REORG and CENSOR_WINDOW

  val treeOk = treeWellFormed(btc)
  val bodiesOk = allBodiesValid(btc)
  val noTwiceOk = noTxTwiceOnBranch(btc)
  val valueOk = valueConserved(btc)
  val withheldOk = withheldOnlyAbovePublished(btc)
  val revealedOk = publishedRevealed(btc)

  val viewsPublished = OBSERVERS.forall(o => btc.blocks.get(views.get(o)).published)

  val honestBlocksPublished =
    btc.blocks.keys().forall(h => btc.blocks.get(h).miner == HONEST implies btc.blocks.get(h).published)

  // the indexer's disconnect/connect stream between ANY two blocks lands on the target branch
  val streamReplayOk =
    btc.blocks.keys().forall(a => btc.blocks.keys().forall(b => streamOk(btc, a, b)))

  val consensusInvariants = and {
    treeOk, bodiesOk, noTwiceOk, valueOk, withheldOk, revealedOk, viewsPublished, honestBlocksPublished,
    streamReplayOk,
  }

  // assumptions: the environment keeps its own premises, so these are no property of Bitcoin

  val reorgDepthBounded = maxReorg <= K_REORG

  val censorshipBounded = {
    val b = bestTip(btc)
    mempool(btc, b).forall(t =>
      validBody(btc, b, Set(t)) implies honestAge(btc, b, bcastHeight.get(t)) <= CENSOR_WINDOW)
  }

  // the censorship window counts from the moment every node knew the transaction
  val bcastKnown = bcastHeight.keys() == btc.broadcast.exclude(Set(GENESIS_TX))

  val assumptionInvariants = and { reorgDepthBounded, censorshipBounded, bcastKnown }

  val allInvariants = and { consensusInvariants, assumptionInvariants }
}
