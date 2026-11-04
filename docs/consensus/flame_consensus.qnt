// -*- mode: Bluespec; -*-
/**
 * Flame minting consensus AS IMPLEMENTED in runflame/flame-lib @ fbd94bc (PR #14).
 *
 * This model deliberately reproduces the current behaviour, bugs included.
 * The invariants at the bottom state what docs/consensus/design-doc.md promises, plus
 * what the code itself needs: a Bitcoin rollback undoes the block (rollbackUndoes,
 * rollbackCommutesWithArrival). Each `run` test in flame_consensus_test.qnt drives the
 * model into a state where one of them is violated, reproducing an issue from the review
 * doc (FL-01 .. FL-08). FL-09 .. FL-12 were found while checking this model
 * against the code and are not in the review doc yet.
 * A fixed protocol should live in a separate module and pass the same invariants.
 *
 * TODO: this model is a hybrid of two PR #14 configurations that no single deployment runs:
 *  - blocks and weights follow demo-consensus: the integrator registers every block with
 *    parent_weight (flame.rs:93-131) and the root is a core block;
 *  - minters follow EndorsementPolicy::Auto: MinterWorker + Endorser + InMemoryMinterJournal.
 *    demo-consensus runs EndorsementPolicy::Manual instead: no worker, endorsements are sent
 *    by hand straight through the sender, bypassing the journal (session/mod.rs:198-208).
 *  Either bring the model to one configuration, or make the minter mode a parameter
 *  (Auto: once per tip with the journal; Manual: any block at any time, no journal).
 *
 * Abstractions (all deliberate, all listed):
 *  - Burns are reduced to a constant minting power per minter, i.e. floor(amount / duration)
 *    of an always-active burn. "x" is an identity with no burns at all.
 *  - Block hashes are integers and "smaller hash" = smaller id. Adversary blocks get ids
 *    below 100, honest ones 100+: Flame blocks need no work, so the adversary can always
 *    grind a smaller hash than any honest block it has seen.
 *  - PR #14 has no BFT. In demo-consensus every block is a core block with a user-chosen
 *    target and core height = parent's + 1, and the root is a core block with core height 1
 *    (demo-consensus flame.rs:45-48, 64-92). The model keeps that root, but the BFT is taken
 *    from the docs, not from code: an honest proposer that extends its own last block, restarts
 *    from node n1's canonical tip when fork choice moves to another branch (design-doc.md,
 *    "Required Properties"), may also produce regular blocks, and sets target = BTC tip + 2
 *    (core-blocks.md).
 *  - Honest minters act on node n1's view, like a minter attached to a local node, once per
 *    BTC tip (minter_worker.rs:44-47): a core block that shows up later at the same tip is
 *    not endorsed until the next tip.
 *  - Bitcoin is bitcoin.qnt (instance B), each node's bitcoind is one of its observers.
 *    K_REORG = 2. CENSOR_WINDOW is off: miners may delay (censor) any
 *    endorsement for any time. An endorsement is a B transaction; `w.payload` keeps what
 *    it means. Each minter starts with one genesis coin. An honest minter funds an
 *    endorsement like fund_transaction_from_script: a confirmed output of its script,
 *    change back to it (transaction_builder.rs). The adversary spends any output it has,
 *    spent or not, and may withhold the transaction.
 *    On a reorg a node rolls its blocks back one by one, then applies the new ones
 *    (ConsensusEvent::BitcoinReorg, consensus_manager.rs:370-384).
 *  - Rewards (FL-04) are not modelled: Quint has no floats. Only the halving constant is checked.
 *  - MAX_DELAY is 2 instead of 10 (orchestrator/defaults.rs) to keep traces short;
 *    the logic does not depend on the value.
 */
module flame_minting {

  // ------------------------------------------------------------------
  // Types
  // ------------------------------------------------------------------

  type BlockId = int

  type Block = {
    id: BlockId,
    parent: BlockId,
    height: int,        // Flame block height
    honest: bool,       // produced by honest BFT (model bookkeeping only)
    isCore: bool,       // header.core_block.is_some()
    coreHeight: int,    // CoreBlockHeader.height      -- NOT validated (FL-01)
    target: int,        // CoreBlockHeader.target_btc_height -- NOT validated (FL-01)
  }

  // MintingEndorsementData::V1 { flame_block_height, flame_block_hash } plus inclusion height
  // (-1 in `w.payload`: a transaction's height depends on the branch).
  type Endorsement = { minter: str, blockId: BlockId, coreHeight: int, incl: int }

  // WeightedBlockHeader { parent_weight, effective_power }
  type WeightRec = { parentWeight: int, effPower: int }

  // What one node (ConsensusStorage + ChainStorage + cursor) holds.
  type NodeState = {
    cursor: int,                              // last processed Bitcoin block (B hash)
    known: Set[BlockId],                      // ChainStorage
    importOrder: List[BlockId],               // order used by CoreBlockSource::get_core_block
    firstSeenAt: BlockId -> int,              // cursor when the block arrived (model bookkeeping)
    stored: (str, int) -> Endorsement,        // EndorsementStorage.endorsements keyed by (minter, core height)
    storedEff: (str, int) -> int,             // effective power stored with each endorsement
    pending: Set[Endorsement],                // EndorsementStorage.pending_endorsements
    doubleSigned: Set[str],                   // DoubleSignStorage: any entry = banned forever
    dsEvidence: Set[Endorsement],             // DoubleSign.endorsements of every entry
    banAt: str -> int,                        // BTC height of the minter's first double sign
    weights: BlockId -> WeightRec,            // WeightedHeaderStorage
  }

  type World = {
    payload: int -> Endorsement,              // B txid -> the endorsement it carries
    blocks: BlockId -> Block,                 // every Flame block ever created
    nextId: int,
    bft: BlockId,                             // BFT's own last block
  }

  // InMemoryMinterJournal: core height -> endorsed block; lost on restart.
  // lastTip: MinterWorker::run last_tip, a B hash (BtcBlockTip compares height and hash),
  // -1 = None.
  // missed: target heights for which the worker found no core block (model bookkeeping, FL-09).
  // unsent: core heights reserved in the journal whose transaction was never sent
  // (EndorsementState::Reserved).
  type MinterState = {
    journal: int -> BlockId, conflicts: Set[int], lastTip: int, missed: Set[int], unsent: Set[int],
  }

  // ------------------------------------------------------------------
  // Parameters
  // ------------------------------------------------------------------

  // Tmp params for fixes; all false = the code.
  // FL-01: a core block's target is above the last core target of its chain.
  const FIX_FL01: bool
  // FL-02: the minter journal is persistent: a restart keeps it.
  const FIX_FL02: bool
  // FL-03: a minter is banned from the Bitcoin height of its first double sign on;
  // its endorsements included earlier stay valid, except at the double-signed core height.
  const FIX_FL03: bool
  // FL-05: block weights are compared exactly, not as floor(log2) (decimals in the real fix;
  // the model takes the exact value, so rounding issues of the real fix are not covered).
  const FIX_FL05: bool
  // FL-10: an honest minter may also spend the change of its own unconfirmed endorsements.
  const FIX_FL10: bool

  pure val HONEST = Set("h1", "h2")
  pure val ADVERSARY = Set("a")
  pure val NO_BURN = Set("x")
  pure val MINTERS = HONEST.union(ADVERSARY).union(NO_BURN)

  // Honest 7, adversary 4: the adversary holds 4/11 = 36% of the power.
  pure val POWER: str -> int = Map("h1" -> 4, "h2" -> 3, "a" -> 4, "x" -> 0)

  pure val NODES = Set("n1", "n2")

  pure val MAX_DELAY = 2            // code: max_endorsement_delay = 10
  pure val MIN_DURATION = 1         // code: orchestrator/defaults.rs:58
  pure val MATURITY = 1             // code: orchestrator/defaults.rs:56
  pure val RATIONALE_MIN_DURATION = 2048   // design-doc.md:324

  pure val MAX_BTC = 6
  pure val MAX_BLOCKS = 8
  pure val MAX_CORE = 5
  pure val GHOST = 999              // a block id that is never created
  pure val GENESIS = 0
  pure val MAX_ENDORSEMENTS = 8     // Bitcoin transactions

  import bitcoin(
    OBSERVERS = NODES,
    OWNERS = MINTERS,
    K_REORG = 2,
    CENSOR_WINDOW = MAX_BTC + 1,    // off
    MAX_BLOCKS = MAX_BTC + 1,       // with genesis
    MAX_TXS = MAX_ENDORSEMENTS,
  ) as B from "./bitcoin"

  // ------------------------------------------------------------------
  // Pure helpers
  // ------------------------------------------------------------------

  // u64::checked_ilog2().unwrap_or(0)
  pure def ilog2(x: int): int =
    if (x <= 0) 0
    else 0.to(62).fold(0, (acc, i) => if (2^i <= x and i > acc) i else acc)

  // endorsement_weight.rs: power at target >> delay, zero past max delay
  pure def effPower(m: str, target: int, incl: int): int =
    if (incl - target > MAX_DELAY) 0 else POWER.get(m) / 2^(incl - target)

  pure def ancestorsOrSelf(blocks: BlockId -> Block, b: BlockId): Set[BlockId] =
    0.to(MAX_BLOCKS).fold(Set(b), (acc, _i) =>
      acc.union(acc.map(x => blocks.get(x).parent).filter(p => p >= 0)))

  pure def descendants(blocks: BlockId -> Block, known: Set[BlockId], b: BlockId): Set[BlockId] =
    known.filter(d => d != b and ancestorsOrSelf(blocks, d).contains(b))

  // nearest core ancestor-or-self, -1 if none
  pure def lastCore(blocks: BlockId -> Block, b: BlockId): BlockId =
    ancestorsOrSelf(blocks, b).filter(x => blocks.get(x).isCore).fold(-1, (best, x) =>
      if (best == -1) x
      else if (blocks.get(x).height > blocks.get(best).height) x
      else best)

  pure def lastCoreHeight(blocks: BlockId -> Block, b: BlockId): int = {
    val c = lastCore(blocks, b)
    if (c == -1) 0 else blocks.get(c).coreHeight
  }

  pure def lastCoreTarget(blocks: BlockId -> Block, b: BlockId): int = {
    val c = lastCore(blocks, b)
    if (c == -1) 0 else blocks.get(c).target
  }

  // weighted_header_storage.rs:50-57; FIX_FL05: exact, see exactWeight
  pure def chainWeight(blocks: BlockId -> Block, n: NodeState, b: BlockId): int =
    if (FIX_FL05) exactWeight(blocks, n, b)
    else {
      val rec = n.weights.get(b)
      rec.parentWeight + (if (blocks.get(b).isCore) ilog2(rec.effPower) else 0)
    }

  // FIX_FL05: the sum of log2(effective power) over the chain's core blocks, compared without
  // rounding: 2^weight = the product of the powers (a power of 0 or 1 adds log2 = 0, like
  // checked_ilog2().unwrap_or(0))
  pure def exactWeight(blocks: BlockId -> Block, n: NodeState, b: BlockId): int =
    ancestorsOrSelf(blocks, b).filter(x => blocks.get(x).isCore).fold(1, (acc, x) => {
      val p = n.weights.get(x).effPower
      acc * (if (p > 1) p else 1)
    })

  // weighted_header_storage.rs:42-47: max weight, ties -> smallest hash; ALL blocks are candidates
  pure def canonicalTip(blocks: BlockId -> Block, n: NodeState): BlockId =
    n.known.fold(-1, (best, b) =>
      if (best == -1) b
      else if (chainWeight(blocks, n, b) > chainWeight(blocks, n, best)) b
      else if (chainWeight(blocks, n, b) == chainWeight(blocks, n, best) and b < best) b
      else best)

  pure def isAncestorOrSelf(blocks: BlockId -> Block, a: BlockId, b: BlockId): bool =
    ancestorsOrSelf(blocks, b).contains(a)

  // flame-chain-service in_memory.rs:78-92: FIRST imported block with that target, canonical or not
  pure def firstImportedWithTarget(blocks: BlockId -> Block, n: NodeState, target: int): BlockId =
    n.importOrder.foldl(-1, (acc, b) =>
      if (acc != -1) acc
      else if (blocks.get(b).isCore and blocks.get(b).target == target) b
      else -1)

  pure def keyOf(e: Endorsement): (str, int) = (e.minter, e.coreHeight)

  // ChainStorage::get_core_block_header: known AND its core height matches the endorsement
  pure def isKnownCore(blocks: BlockId -> Block, n: NodeState, e: Endorsement): bool =
    if (n.known.contains(e.blockId))
      blocks.get(e.blockId).isCore and blocks.get(e.blockId).coreHeight == e.coreHeight
    else false

  // endorsement_validator.rs:69-84: unknown block -> pending, no power check (FL-07)
  // banned now (FL-03), or (FIX_FL03) at the endorsement's inclusion height
  pure def bannedFor(n: NodeState, e: Endorsement): bool =
    n.doubleSigned.contains(e.minter) and (not(FIX_FL03) or n.banAt.get(e.minter) <= e.incl)

  pure def isPendingE(blocks: BlockId -> Block, n: NodeState, e: Endorsement): bool =
    not(isKnownCore(blocks, n, e)) and not(bannedFor(n, e))

  // endorsement_validator.rs:85-104: target reached, not banned NOW (FL-03), has burns
  pure def isEligible(blocks: BlockId -> Block, n: NodeState, e: Endorsement): bool =
    if (isKnownCore(blocks, n, e))
      and {
        e.incl >= blocks.get(e.blockId).target,
        not(bannedFor(n, e)),
        POWER.get(e.minter) > 0,
      }
    else false

  // record_endorsement: the first one (lowest inclusion height) wins
  pure def firstOf(es: Set[Endorsement]): Endorsement =
    es.fold({ minter: "", blockId: -1, coreHeight: -1, incl: 1000000 }, (a, e) =>
      if (e.incl < a.incl) e else a)

  // block hashes seen for (minter, core height): this batch plus what is stored
  pure def hashesAt(n: NodeState, eligible: Set[Endorsement], k: (str, int)): Set[BlockId] =
    eligible.filter(e => keyOf(e) == k).map(e => e.blockId)
      .union(if (n.stored.keys().contains(k)) Set(n.stored.get(k).blockId) else Set())

  pure def deltaFor(
    blocks: BlockId -> Block, n: NodeState,
    accepted: Set[Endorsement], removedKeys: Set[(str, int)], b: BlockId
  ): int = {
    val added = accepted.filter(e => e.blockId == b)
      .fold(0, (s, e) => s + effPower(e.minter, blocks.get(b).target, e.incl))
    val removed = removedKeys.filter(k => n.stored.get(k).blockId == b)
      .fold(0, (s, k) => s + n.storedEff.get(k))
    added - removed
  }

  // weighter.rs:51-74: new effective power, log2 delta pushed into every known descendant
  pure def applyDelta(
    blocks: BlockId -> Block, known: Set[BlockId],
    ws: BlockId -> WeightRec, b: BlockId, d: int
  ): BlockId -> WeightRec = {
    val oldP = ws.get(b).effPower
    val newP = oldP + d
    // FIX_FL05: chain weights are computed from the powers, parentWeight is not used
    val dl = if (FIX_FL05) 0 else ilog2(newP) - ilog2(oldP)
    val ws1 = ws.set(b, { ...ws.get(b), effPower: newP })
    descendants(blocks, known, b).fold(ws1, (acc, x) =>
      acc.set(x, { ...acc.get(x), parentWeight: acc.get(x).parentWeight + dl }))
  }

  pure val NEVER = 1000000

  pure def minInt(a: int, b: int): int = if (a < b) a else b

  // FIX_FL03: the inclusion height at which core height k got its second hash
  pure def banPoint(n: NodeState, eligible: Set[Endorsement], k: (str, int)): int = {
    val es = eligible.filter(e => keyOf(e) == k)
      .union(if (n.stored.keys().contains(k)) Set(n.stored.get(k)) else Set())
    val first = es.map(e => e.blockId).map(b => es.filter(e => e.blockId == b).fold(NEVER, (acc, e) => minInt(acc, e.incl)))
    val m1 = first.fold(NEVER, (acc, x) => minInt(acc, x))
    val tied = es.map(e => e.blockId).filter(b => es.filter(e => e.blockId == b).fold(NEVER, (acc, e) => minInt(acc, e.incl)) == m1)
    if (tied.size() > 1) m1 else first.exclude(Set(m1)).fold(NEVER, (acc, x) => minInt(acc, x))
  }

  // a disqualified minter loses all its endorsements in the batch (code: endorsement_validator.rs
  // 141-150), or (FIX_FL03) only those included at or after its ban height
  pure def dropped(e: Endorsement, disq: Set[str], banAfter: str -> int): bool =
    if (FIX_FL03) banAfter.keys().contains(e.minter) and e.incl >= banAfter.get(e.minter)
    else disq.contains(e.minter)

  /// EndorsementValidator + Weighter + ConsensusOperations on one batch of endorsements:
  /// a Bitcoin block, or the pending endorsements of a block that just arrived.
  pure def processBatch(blocks: BlockId -> Block, n: NodeState, batch: Set[Endorsement]): NodeState = {
    val eligible = batch.filter(e => isEligible(blocks, n, e))
    val pendingNew = batch.filter(e => isPendingE(blocks, n, e))
    val eKeys = eligible.map(e => keyOf(e))
    val storedKeys = n.stored.keys()
    // double sign = same minter, same CORE HEIGHT, different hash (FL-02)
    val dsKeys = eKeys.filter(k => hashesAt(n, eligible, k).size() > 1)
    val disq = dsKeys.map(k => k._1)
    // FIX_FL03: the ban starts at the earliest height where some core height has two hashes
    val banAfter = n.doubleSigned.union(disq).mapBy(m => {
      val fresh = dsKeys.filter(k => k._1 == m).fold(NEVER, (acc, k) => minInt(acc, banPoint(n, eligible, k)))
      if (n.doubleSigned.contains(m)) minInt(n.banAt.get(m), fresh) else fresh
    })
    // FIX_FL03: stored endorsements included at or after the ban lose their weight too
    val lateKeys = if (FIX_FL03)
        storedKeys.filter(k => banAfter.keys().contains(k._1) and n.stored.get(k).incl >= banAfter.get(k._1))
      else Set()
    // only the stored endorsement at the conflicting height is removed;
    // the minter's endorsements at other heights keep their weight (FL-03)
    val removedKeys = dsKeys.filter(k => storedKeys.contains(k)).union(lateKeys)
    val accKeys = eKeys.filter(k => and {
      not(dsKeys.contains(k)),
      not(dropped(firstOf(eligible.filter(e => keyOf(e) == k)), disq, banAfter)),
      not(storedKeys.contains(k)),
      // FIX_FL03: a double-signed core height stays dropped, also for an endorsement
      // included before the ban that reaches the node late
      not(FIX_FL03) or n.dsEvidence.forall(e => keyOf(e) != k),
    })
    val accepted = accKeys.map(k => firstOf(eligible.filter(e => keyOf(e) == k)))
    val affected = removedKeys.map(k => n.stored.get(k).blockId).union(accepted.map(e => e.blockId))
    val weights1 = affected.fold(n.weights, (ws, b) =>
      applyDelta(blocks, n.known, ws, b, deltaFor(blocks, n, accepted, removedKeys, b)))
    // HeightEndorsements::record_endorsement: every endorsement at the height, stored one first
    val evidence = eligible.filter(e => dsKeys.contains(keyOf(e)))
      .union(dsKeys.filter(k => storedKeys.contains(k)).map(k => n.stored.get(k)))
    val keptKeys = storedKeys.exclude(removedKeys)
    val stored1 = accepted.fold(keptKeys.mapBy(k => n.stored.get(k)), (m, e) => m.put(keyOf(e), e))
    val eff1 = accepted.fold(keptKeys.mapBy(k => n.storedEff.get(k)), (m, e) =>
      m.put(keyOf(e), effPower(e.minter, blocks.get(e.blockId).target, e.incl)))
    {
      ...n,
      stored: stored1,
      storedEff: eff1,
      pending:
        if (FIX_FL03) n.pending.exclude(batch).union(pendingNew).filter(e => not(dropped(e, disq, banAfter)))
        else n.pending.exclude(batch).union(pendingNew.filter(e => not(disq.contains(e.minter)))),
      doubleSigned: n.doubleSigned.union(disq),
      dsEvidence: n.dsEvidence.union(evidence),
      banAt: banAfter,
      weights: weights1,
    }
  }

  // Integration code (demo-consensus flame.rs:95-131) registers every block with
  // parent_weight = chain weight of the parent; flame_block_processor.rs:91-110 then
  // re-evaluates the pending endorsements of a core block against the CURRENT state.
  pure def arrive(blocks: BlockId -> Block, n: NodeState, b: BlockId): NodeState = {
    val pw = if (FIX_FL05) 0 else chainWeight(blocks, n, blocks.get(b).parent)
    val n1 = {
      ...n,
      known: n.known.union(Set(b)),
      importOrder: n.importOrder.append(b),
      weights: n.weights.put(b, { parentWeight: pw, effPower: 0 }),
      firstSeenAt: n.firstSeenAt.put(b, n.cursor),
    }
    if (blocks.get(b).isCore)
      processBatch(blocks, n1, n1.pending.filter(e => e.blockId == b and e.coreHeight == blocks.get(b).coreHeight))
    else n1
  }

  // endorsements included in Bitcoin block h
  pure def endorsementsIn(world: World, c: B::Btc, h: int): Set[Endorsement] =
    c.blocks.get(h).txs.filter(t => world.payload.keys().contains(t))
      .map(t => { ...world.payload.get(t), incl: B::heightOf(c, h) })

  // h: the child of the cursor on the node's Bitcoin branch
  pure def processBtcBlock(world: World, c: B::Btc, n: NodeState, h: int): NodeState = {
    val n1 = processBatch(world.blocks, n, endorsementsIn(world, c, h))
    { ...n1, cursor: h }
  }

  pure def ofMinters(es: Set[Endorsement], ms: Set[str]): Set[Endorsement] = es.filter(e => ms.contains(e.minter))

  // BitcoinBlockProcessor::rollback of the cursor block (bitcoin_block_processor.rs:402-483,
  // bitcoin_rollback.rs): every minter with an endorsement in the block loses its stored and
  // pending endorsements and its double signs; everything else of them (every endorsement
  // in the applied blocks, get_applied_endorsements, plus stored, pending and double-sign
  // evidence, minus this block) is evaluated again as one batch.
  pure def rollbackBtcBlock(world: World, c: B::Btc, n: NodeState): NodeState = {
    val h = n.cursor
    val hh = B::heightOf(c, h)
    val ms = endorsementsIn(world, c, h).map(e => e.minter)
    val storedKeys = n.stored.keys().filter(k => ms.contains(k._1))
    val storedMs = storedKeys.map(k => n.stored.get(k))
    val applied = B::chain(c, h).foldl(Set(), (acc, b) => acc.union(endorsementsIn(world, c, b)))
    val remaining = ofMinters(applied, ms).union(storedMs).union(ofMinters(n.pending, ms))
      .union(ofMinters(n.dsEvidence, ms)).filter(e => e.incl != hh)
    // evaluate_with_removals: the removed endorsements give their power back
    val weights0 = storedKeys.map(k => n.stored.get(k).blockId).fold(n.weights, (ws, b) =>
      applyDelta(world.blocks, n.known, ws, b,
        0 - storedKeys.filter(k => n.stored.get(k).blockId == b).fold(0, (sum, k) => sum + n.storedEff.get(k))))
    val kept = n.stored.keys().exclude(storedKeys)
    val n0 = {
      ...n,
      stored: kept.mapBy(k => n.stored.get(k)),
      storedEff: kept.mapBy(k => n.storedEff.get(k)),
      pending: n.pending.exclude(ofMinters(n.pending, ms)),
      doubleSigned: n.doubleSigned.exclude(ms),
      dsEvidence: n.dsEvidence.exclude(ofMinters(n.dsEvidence, ms)),
      banAt: n.banAt.keys().exclude(ms).mapBy(m => n.banAt.get(m)),
      weights: weights0,
    }
    { ...processBatch(world.blocks, n0, remaining), cursor: c.blocks.get(h).parent }
  }

  // what a rollback must restore exactly. Double signs are compared by (minter, core height):
  // a rollback re-evaluates the minter's whole history, so its DoubleSign record may also list
  // later endorsements that the ban made the node ignore. Only storage differs, not weights.
  pure def consensusState(n: NodeState): (
    (str, int) -> Endorsement, (str, int) -> int, Set[Endorsement], Set[str], Set[(str, int)], BlockId -> WeightRec, int,
    str -> int
  ) = (n.stored, n.storedEff, n.pending, n.doubleSigned, n.dsEvidence.map(e => keyOf(e)), n.weights, n.cursor, n.banAt)

  // an output of m that no transaction spends yet: the change of its last endorsement
  pure def walletCoin(c: B::Btc, m: str): (int, int) =
    B::spendableOuts(c, c.txs.keys()).exclude(B::spent(c, c.txs.keys()))
      .filter(op => B::outAt(c, op).owner == m)
      .fold((-1, -1), (_acc, op) => op)

  // what fund_transaction_from_script can spend: listunspent with confirmations > 0 on the
  // minter's script, minus outputs its bitcoind's mempool already spends
  pure def confirmedCoins(c: B::Btc, tip: int, m: str): Set[(int, int)] =
    B::listUnspent(c, tip, m, c.broadcast, 1).union(if (FIX_FL10) ownChange(c, tip, m) else Set())

  // FIX_FL10: the minter's outputs created by its transactions waiting in the mempool and not
  // spent by another one (listunspent minconf=0 for the wallet's own change)
  pure def ownChange(c: B::Btc, tip: int, m: str): Set[(int, int)] = {
    val waiting = B::mempool(c, tip)
    B::spendableOuts(c, waiting).exclude(B::spent(c, waiting)).filter(op => B::outAt(c, op).owner == m)
  }

  // ... and takes the largest (ties: the smallest outpoint)
  pure def pickCoin(c: B::Btc, coins: Set[(int, int)]): (int, int) =
    coins.fold((-1, -1), (best, op) =>
      if (best == (-1, -1)) op
      else if (B::outAt(c, op).value != B::outAt(c, best).value)
        if (B::outAt(c, op).value > B::outAt(c, best).value) op else best
      else if (op._1 < best._1 or (op._1 == best._1 and op._2 < best._2)) op
      else best)

  // ------------------------------------------------------------------
  // State
  // ------------------------------------------------------------------

  var w: World
  var nodes: str -> NodeState
  var minters: str -> MinterState
  // and B::btc, B::views, B::maxReorg, B::bcastHeight

  // demo-consensus root: a core block with core height 1 (target_btc_height is configurable).
  // TODO: the spec does not define genesis (core or not, core height, target, endorsable);
  // sync this with docs/consensus once it does.
  pure val genesisBlock: Block = {
    id: GENESIS, parent: -1, height: 0, honest: true, isCore: true, coreHeight: 1, target: 0,
  }

  pure val emptyNode: NodeState = {
    cursor: B::GENESIS,
    known: Set(GENESIS),
    importOrder: [GENESIS],
    firstSeenAt: Map(GENESIS -> B::GENESIS),
    stored: Map(),
    storedEff: Map(),
    pending: Set(),
    doubleSigned: Set(),
    dsEvidence: Set(),
    banAt: Map(),
    weights: Map(GENESIS -> { parentWeight: 0, effPower: 0 }),
  }

  action init = all {
    B::init,
    w' = { payload: Map(), blocks: Map(GENESIS -> genesisBlock), nextId: 1, bft: GENESIS },
    nodes' = NODES.mapBy(_n => emptyNode),
    minters' = HONEST.mapBy(_m => { journal: Map(), conflicts: Set(), lastTip: B::NO_BLOCK, missed: Set(), unsent: Set() }),
  }

  // the tip of a node's bitcoind; honest minters and the BFT read node n1's
  def btcTip(nd: str): int = B::views.get(nd)

  def btcHeight(nd: str): int = B::heightOf(B::btc, btcTip(nd))

  // ------------------------------------------------------------------
  // Actions
  // ------------------------------------------------------------------

  action bitcoinUnchanged = all {
    B::btc' = B::btc,
    B::views' = B::views,
    B::maxReorg' = B::maxReorg,
    B::bcastHeight' = B::bcastHeight,
  }

  action flameUnchanged = all { w' = w, nodes' = nodes, minters' = minters }

  // Bitcoin on its own: mining, censoring, publishing, bitcoinds following the chain.
  action btcStep = all { B::envStep, flameUnchanged }

  // An honest Bitcoin block on the best tip; endorsements of `censored` minters stay in the
  // mempool. The nodes' bitcoinds do not see it yet (observeBtc).
  action mineBtc(censored: Set[str]): bool = {
    val tip = B::bestTip(B::btc)
    all {
      B::mineHonest(tip, B::mempool(B::btc, tip).filter(t => not(censored.contains(w.payload.get(t).minter)))),
      flameUnchanged,
    }
  }

  // ... on any parent: a fork, if `parent` is not the best tip
  action mineBtcOn(parent: int, censored: Set[str]): bool = all {
    B::mineHonest(parent, B::mempool(B::btc, parent).filter(t => not(censored.contains(w.payload.get(t).minter)))),
    flameUnchanged,
  }

  // A node's bitcoind adopts the best tip.
  action observeBtc(nd: str): bool = all { B::observe(nd, B::bestTip(B::btc)), flameUnchanged }

  // A minter signs `e` spending `coin`, change back to itself, and relays it or keeps it.
  // The caller assigns nodes' and minters'.
  action sendEndorsement(e: Endorsement, coin: (int, int), private: bool): bool = {
    val tx = B::payment(B::btc, coin, e.minter)
    all {
      if (private) B::withholdTx(tx) else B::relayTx(tx),
      w' = { ...w, payload: w.payload.put(tx.id, e) },
    }
  }

  // Honest BFT core block: target = latest BTC height + 2 (core-blocks.md),
  // core height = parent's last core height + 1, target above the chain's last core target.
  // TODO: bftPropose and bftRegular model the BFT from the docs; replace them with the real
  // BFT (IBFT or Simplex) once it lands in code.
  action bftPropose: bool = {
    val n = nodes.get("n1")
    val canon = canonicalTip(w.blocks, n)
    val parent = if (isAncestorOrSelf(w.blocks, canon, w.bft)) w.bft else canon
    val target = btcHeight("n1") + 2
    val id = 100 + w.nextId
    all {
      w.nextId < MAX_BLOCKS,
      lastCoreTarget(w.blocks, parent) < target,
      w' = {
        ...w,
        blocks: w.blocks.put(id, {
          id: id, parent: parent, height: w.blocks.get(parent).height + 1, honest: true,
          isCore: true, coreHeight: lastCoreHeight(w.blocks, parent) + 1, target: target,
        }),
        nextId: w.nextId + 1,
        bft: id,
      },
      nodes' = nodes,
      minters' = minters,
      bitcoinUnchanged,
    }
  }

  // Honest BFT regular block.
  action bftRegular: bool = {
    val n = nodes.get("n1")
    val canon = canonicalTip(w.blocks, n)
    val parent = if (isAncestorOrSelf(w.blocks, canon, w.bft)) w.bft else canon
    val id = 100 + w.nextId
    all {
      w.nextId < MAX_BLOCKS,
      w' = {
        ...w,
        blocks: w.blocks.put(id, {
          id: id, parent: parent, height: w.blocks.get(parent).height + 1, honest: true,
          isCore: false, coreHeight: 0, target: 0,
        }),
        nextId: w.nextId + 1,
        bft: id,
      },
      nodes' = nodes,
      minters' = minters,
      bitcoinUnchanged,
    }
  }

  // minter_worker.rs + endorser.rs: once per BTC tip t, endorse the block with target t + 1.
  // The journal is keyed by core height; a different hash at a used height is refused.
  // TODO: once LATE_MINTING_POLICY (core-blocks.md step 3) or wait_core_block is implemented,
  // let the minter endorse a core block that arrives after its tip was processed (FL-09).
  action honestEndorse(m: str): bool = {
    val n = nodes.get("n1")
    val tip = btcTip("n1")
    val target = btcHeight("n1") + 1
    val bid = firstImportedWithTarget(w.blocks, n, target)
    val ch = if (bid >= 0) w.blocks.get(bid).coreHeight else -1
    val prev = minters.get(m)
    val ms = { ...prev, lastTip: tip, missed: prev.missed.exclude(Set(target)) }
    val reserved = ms.journal.keys().contains(ch)
    all {
      prev.lastTip != tip,
      nodes' = nodes,
      if (bid < 0)
        // "No core block for the BTC target height": the tip is used up, no retry (FL-09)
        all { minters' = minters.set(m, { ...ms, missed: ms.missed.union(Set(target)) }), w' = w, bitcoinUnchanged }
      else if (reserved and ms.journal.get(ch) == bid)
        // AlreadyReserved: nothing to do
        all { minters' = minters.set(m, ms), w' = w, bitcoinUnchanged }
      else if (reserved)
        // InMemoryMinterJournal::ConflictingEndorsement: logged, minter abstains (FL-02)
        all {
          minters' = minters.set(m, { ...ms, conflicts: ms.conflicts.union(Set(ch)) }),
          w' = w,
          bitcoinUnchanged,
        }
      else {
        val coins = confirmedCoins(B::btc, tip, m)
        val reservedMs = { ...ms, journal: ms.journal.put(ch, bid) }
        if (coins == Set())
          // write_reserve_endorsement succeeded, send_endorsement fails ("no confirmed safe
          // UTXOs"): the reservation stays, the worker moves on, AlreadyReserved never resends
          all {
            minters' = minters.set(m, { ...reservedMs, unsent: ms.unsent.union(Set(ch)) }),
            w' = w,
            bitcoinUnchanged,
          }
        else
          all {
            minters' = minters.set(m, reservedMs),
            sendEndorsement({ minter: m, blockId: bid, coreHeight: ch, incl: -1 }, pickCoin(B::btc, coins), false),
          }
      },
    }
  }

  // The journal lives in memory: a restart forgets every reservation (FL-02),
  // and the new worker processes the current tip again.
  action restartMinter(m: str): bool = all {
    minters.get(m).journal.keys().size() > 0,
    minters' = minters.set(m,
      if (FIX_FL02) { ...minters.get(m), lastTip: B::NO_BLOCK }
      else { ...minters.get(m), journal: Map(), conflicts: Set(), lastTip: B::NO_BLOCK, unsent: Set() }),
    w' = w,
    nodes' = nodes,
    bitcoinUnchanged,
  }

  // Anyone may build a block anywhere, with any core metadata (design-doc.md:444, FL-01).
  action advBlock(parent: BlockId, isCore: bool, ch: int, target: int): bool = all {
    w.nextId < MAX_BLOCKS,
    w.blocks.keys().contains(parent),
    w' = {
      ...w,
      blocks: w.blocks.put(w.nextId, {
        id: w.nextId, parent: parent, height: w.blocks.get(parent).height + 1, honest: false,
        isCore: isCore, coreHeight: if (isCore) ch else 0, target: if (isCore) target else 0,
      }),
      nextId: w.nextId + 1,
    },
    nodes' = nodes,
    minters' = minters,
    bitcoinUnchanged,
  }

  // Adversary (or a burn-less identity) endorses any (core height, hash), known or not,
  // with any output it has: a spent one makes a conflicting transaction.
  action advEndorseWith(m: str, coin: (int, int), private: bool, bid: BlockId, ch: int): bool = all {
    B::outAt(B::btc, coin).owner == m,
    sendEndorsement({ minter: m, blockId: bid, coreHeight: ch, incl: -1 }, coin, private),
    nodes' = nodes,
    minters' = minters,
  }

  // ... relayed, spending the change of its last endorsement
  action advEndorse(m: str, bid: BlockId, ch: int): bool =
    advEndorseWith(m, walletCoin(B::btc, m), false, bid, ch)

  // Gossip: any created block may reach any node at any time, or never (withholding).
  action deliver(nd: str, b: BlockId): bool = all {
    w.blocks.keys().contains(b),
    not(nodes.get(nd).known.contains(b)),
    nodes.get(nd).known.contains(w.blocks.get(b).parent),
    // FIX_FL01: the node rejects a core block whose target does not move forward
    not(FIX_FL01) or not(w.blocks.get(b).isCore)
      or w.blocks.get(b).target > lastCoreTarget(w.blocks, w.blocks.get(b).parent),
    nodes' = nodes.set(nd, arrive(w.blocks, nodes.get(nd), b)),
    w' = w,
    minters' = minters,
    bitcoinUnchanged,
  }

  // The indexer hands the node the next step towards its bitcoind's tip: roll back the
  // cursor block while it is off that branch, then apply the next block of the branch.
  action processBtc(nd: str): bool = {
    val n = nodes.get(nd)
    val tip = btcTip(nd)
    all {
      n.cursor != tip,
      nodes' = nodes.set(nd,
        if (B::isAncestor(B::btc, n.cursor, tip))
          processBtcBlock(w, B::btc, n, B::chain(B::btc, tip)[B::heightOf(B::btc, n.cursor) + 1])
        else rollbackBtcBlock(w, B::btc, n)),
      w' = w,
      minters' = minters,
      bitcoinUnchanged,
    }
  }

  action step = any {
    btcStep,
    bftPropose,
    bftRegular,
    nondet m = HONEST.oneOf()
    honestEndorse(m),
    nondet m = HONEST.oneOf()
    restartMinter(m),
    nondet parent = w.blocks.keys().oneOf()
    nondet isCore = Set(true, false).oneOf()
    nondet ch = 0.to(MAX_CORE).oneOf()
    nondet target = 0.to(MAX_BTC + 2).oneOf()
    advBlock(parent, isCore, ch, target),
    nondet m = ADVERSARY.union(NO_BURN).oneOf()
    nondet bid = w.blocks.keys().union(Set(GHOST)).oneOf()
    nondet ch = 0.to(MAX_CORE).oneOf()
    nondet coin = B::allOutpoints(B::btc).oneOf()
    nondet private = Set(true, false).oneOf()
    advEndorseWith(m, coin, private, bid, ch),
    nondet nd = NODES.oneOf()
    nondet b = w.blocks.keys().oneOf()
    deliver(nd, b),
    nondet nd = NODES.oneOf()
    processBtc(nd),
  }

  // ------------------------------------------------------------------
  // Properties promised by the design doc (all expected to FAIL here)
  // ------------------------------------------------------------------

  // FL-01: on the canonical chain core targets strictly increase (one core block per BTC slot).
  val canonicalSlotsIncrease: bool = NODES.forall(nd => {
    val n = nodes.get(nd)
    val cores = ancestorsOrSelf(w.blocks, canonicalTip(w.blocks, n)).filter(b => w.blocks.get(b).isCore)
    cores.forall(x => cores.forall(y =>
      (w.blocks.get(x).height < w.blocks.get(y).height) implies
        (w.blocks.get(x).target < w.blocks.get(y).target)))
  })

  pure def endorsedOnTimeByAllHonest(blocks: BlockId -> Block, n: NodeState, b: BlockId): bool =
    HONEST.forall(m => {
      val k = (m, blocks.get(b).coreHeight)
      if (n.stored.keys().contains(k))
        n.stored.get(k).blockId == b and n.stored.get(k).incl == blocks.get(b).target
      else false
    })

  // FL-01, FL-05: a 36% adversary cannot displace an honest core block
  // that every honest minter endorsed with zero delay.
  val honestCoreBlocksStay: bool = NODES.forall(nd => {
    val n = nodes.get(nd)
    val tip = canonicalTip(w.blocks, n)
    n.known
      .filter(b => w.blocks.get(b).honest and w.blocks.get(b).isCore and endorsedOnTimeByAllHonest(w.blocks, n, b))
      .forall(b => isAncestorOrSelf(w.blocks, b, tip))
  })

  // FL-02: an honest minter following fork choice is never blocked by its own journal...
  val honestMintersNeverBlocked: bool = HONEST.forall(m => minters.get(m).conflicts.size() == 0)

  // FL-02: ...and never gets banned for double signing.
  val honestNeverDoubleSigned: bool =
    NODES.forall(nd => HONEST.forall(m => not(nodes.get(nd).doubleSigned.contains(m))))

  // FL-03: two nodes that saw the same data compute the same weights and the same tip.
  val determinism: bool = {
    val a = nodes.get("n1")
    val b = nodes.get("n2")
    (a.cursor == b.cursor and a.known == b.known) implies
      (a.weights == b.weights and canonicalTip(w.blocks, a) == canonicalTip(w.blocks, b))
  }

  // FL-07: pending endorsements come from minters with power and expire after MAX_DELAY.
  // Disputed (PR #49): the author holds the attack too costly, one Bitcoin transaction
  // per pending record.
  val pendingBounded: bool =
    NODES.forall(nd => nodes.get(nd).pending.forall(e =>
      POWER.get(e.minter) > 0 and B::heightOf(B::btc, nodes.get(nd).cursor) - e.incl <= MAX_DELAY))

  // FL-08: design-doc.md:144 -- a block that shows up long after its slot cannot gain weight.
  // Disputed (PR #49): the author reads design-doc.md:14 as limiting delayed endorsements
  // only, and holds a delayed block reveal an acceptable risk.
  val lateBlocksGetNoWeight: bool = NODES.forall(nd => {
    val n = nodes.get(nd)
    n.known.forall(b =>
      if (w.blocks.get(b).isCore and B::heightOf(B::btc, n.firstSeenAt.get(b)) - w.blocks.get(b).target > MAX_DELAY)
        n.weights.get(b).effPower == 0
      else true)
  })

  // FL-09: core-blocks.md step 3 -- an honest minter that found no core block for its tip
  // still endorses it once it arrives; it never ends up holding a missed slot whose honest
  // core block node n1 knows by now.
  // Disputed (PR #49): the author calls endorsing only an already known core block the
  // simplest strategy, since a late endorsement earns less.
  val honestMintersNeverMissSlot: bool = {
    val n = nodes.get("n1")
    HONEST.forall(m => minters.get(m).missed.forall(t =>
      n.known.forall(b => not(and {
        w.blocks.get(b).honest,
        w.blocks.get(b).isCore,
        w.blocks.get(b).target == t,
      }))))
  }

  // FL-10: an honest minter sends every endorsement it reserves.
  // fund_transaction_from_script spends confirmed outputs only (transaction_builder.rs:106)
  // and returns the change to the same script, so while the minter's last endorsement waits
  // for a block it has nothing to spend. Endorser::endorse has already reserved the core
  // height (endorser.rs:45-55), the worker logs the error and waits for the next tip
  // (minter_worker.rs:99-100), and a reserved height is never sent again
  // (in_memory_minter_journal.rs:86-88).
  // A failed send after a restart is harmless when an earlier transaction carries the same
  // endorsement.
  val honestEndorsementsSent: bool = HONEST.forall(m => {
    val ms = minters.get(m)
    ms.unsent.forall(ch => w.payload.keys().exists(t =>
      w.payload.get(t) == { minter: m, blockId: ms.journal.get(ch), coreHeight: ch, incl: -1 }))
  })

  // for tests, which cannot see B
  val B_maxReorg: int = B::maxReorg
  def onBtcBranch(nd: str): bool = B::isAncestor(B::btc, nodes.get(nd).cursor, btcTip(nd))
  def nextBlockMinters(nd: str): Set[str] =
    B::publishedBlocks(B::btc).filter(h => B::btc.blocks.get(h).parent == nodes.get(nd).cursor)
      .fold(Set(), (acc, h) => acc.union(endorsementsIn(w, B::btc, h).map(e => e.minter)))

  // Rolling back a Bitcoin block undoes it: a node that applied the next block of any
  // branch and rolled it back is where it was (ConsensusUndo).
  val rollbackUndoes: bool = NODES.forall(nd => {
    val n = nodes.get(nd)
    B::publishedBlocks(B::btc).filter(h => B::btc.blocks.get(h).parent == n.cursor).forall(h =>
      consensusState(rollbackBtcBlock(w, B::btc, processBtcBlock(w, B::btc, n, h))) == consensusState(n))
  })

  // ... also when a Flame block arrives in between: the node ends where a node that never
  // saw the Bitcoin block is after the same arrival.
  val rollbackCommutesWithArrival: bool = NODES.forall(nd => {
    val n = nodes.get(nd)
    B::publishedBlocks(B::btc).filter(h => B::btc.blocks.get(h).parent == n.cursor).forall(h =>
      w.blocks.keys().filter(b => not(n.known.contains(b)) and n.known.contains(w.blocks.get(b).parent))
        .forall(b =>
          consensusState(rollbackBtcBlock(w, B::btc, arrive(w.blocks, processBtcBlock(w, B::btc, n, h), b)))
            == consensusState(arrive(w.blocks, n, b))))
  })

  // FL-11: both properties above fail when the rolled-back block holds an endorsement of a
  // minter banned by an earlier block: the rollback evaluates the minter's stored endorsements
  // and its double-sign evidence as one batch, and a double sign anywhere in a batch drops
  // all of the minter's endorsements in it (endorsement_validator.rs:141-150).

  // FL-06: the security argument assumes burns last at least 2048 blocks.
  val paramsMatchRationale: bool = MIN_DURATION >= RATIONALE_MIN_DURATION

  val allInvariants: bool = and {
    canonicalSlotsIncrease,
    honestCoreBlocksStay,
    honestMintersNeverBlocked,
    honestNeverDoubleSigned,
    determinism,
    pendingBounded,
    lateBlocksGetNoWeight,
    honestMintersNeverMissSlot,
    honestEndorsementsSent,
    rollbackUndoes,                 // FL-11
    rollbackCommutesWithArrival,    // FL-11
  }

  // Bitcoin's own invariants still hold with Flame on top: they must never fail.
  val bitcoinOk: bool = B::allInvariants
}
