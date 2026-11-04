// -*- mode: Bluespec; -*-
/**
 * Reproductions for flame_consensus.qnt: each run ends in a state that violates one
 * promise of the design doc or one property the code needs (FL-01 .. FL-12), plus
 * parameter checks and the normal scenario.
 *   quint test flame_consensus_test.qnt
 */
module flame_consensus_test {
  // the code as it is: no fixes
  import flame_minting(FIX_FL01 = false, FIX_FL02 = false, FIX_FL03 = false, FIX_FL05 = false, FIX_FL10 = false).* from "./flame_consensus"

  // An honest Bitcoin block that both nodes' bitcoinds see at once.
  run btcBlock(censored: Set[str]): bool =
    mineBtc(censored).then(observeBtc("n1")).then(observeBtc("n2"))

  // FL-01: three core blocks on one target outweigh an honest block with 7/11 of the power.
  run fl01CoreSpamTest =
    init
      .then(advBlock(GENESIS, true, 2, 1))                // id 1
      .then(advBlock(1, true, 3, 1))                      // id 2, same target
      .then(advBlock(2, true, 4, 1))                      // id 3, same target
      .then(advEndorse("a", 1, 2))
      .then(advEndorse("a", 2, 3))
      .then(advEndorse("a", 3, 4))
      .then(bftPropose)                                   // id 104: core 2, target 2
      .then(deliver("n1", 104))
      .then(deliver("n1", 1))
      .then(deliver("n1", 2))
      .then(deliver("n1", 3))
      .then(btcBlock(Set()))                               // BTC 1: adversary endorsements
      .then(honestEndorse("h1"))
      .then(honestEndorse("h2"))
      .then(btcBlock(Set()))                               // BTC 2: honest endorsements, delay 0
      .then(processBtc("n1"))
      .then(processBtc("n1"))
      .expect(and {
        canonicalTip(w.blocks, nodes.get("n1")) == 3,     // weight 6 vs 2
        not(canonicalSlotsIncrease),
        not(honestCoreBlocksStay),
      })

  // FL-05: same slot, valid metadata, 4 vs 7 power -> both weigh floor(log2) = 2;
  // the adversary's smaller hash wins the tie.
  run fl05TieGrindTest =
    init
      .then(bftPropose)                                   // id 101: core 2, target 2
      .then(deliver("n1", 101))
      .then(advBlock(GENESIS, true, 2, 2))                // id 2: core 2, target 2
      .then(deliver("n1", 2))
      .then(btcBlock(Set()))                               // BTC 1
      .then(honestEndorse("h1"))
      .then(honestEndorse("h2"))
      .then(advEndorse("a", 2, 2))
      .then(btcBlock(Set()))                               // BTC 2: everyone on time
      .then(processBtc("n1"))
      .then(processBtc("n1"))
      .expect(and {
        canonicalTip(w.blocks, nodes.get("n1")) == 2,
        canonicalSlotsIncrease,                           // not FL-01: metadata is valid
        not(honestCoreBlocksStay),
      })

  // FL-02: Bitcoin miners delay honest endorsements, a valid adversary branch with fewer
  // core blocks wins, BFT restarts on it and reuses core height 3. The honest journal refuses
  // to endorse; after a restart the minter endorses and is banned forever.
  // h1 has one coin: its endorsement of A1 must confirm before it can fund the one of A2.
  run fl02HonestSlashedTest =
    init
      .then(bftPropose)                                   // A1 = 101: core 2, target 2
      .then(deliver("n1", 101))
      .then(btcBlock(Set()))                              // BTC 1
      .then(honestEndorse("h1"))                          // journal: 2 -> 101
      .then(honestEndorse("h2"))
      .then(bftPropose)                                   // A2 = 102: core 3, target 3
      .then(deliver("n1", 102))
      .then(advBlock(GENESIS, true, 2, 3))                // B1 = 3: core 2, target 3
      .then(deliver("n1", 3))
      .then(btcBlock(Set("h2")))                          // BTC 2: h1 on time, h2 delayed
      .then(honestEndorse("h1"))                          // journal: 3 -> 102, sent
      .then(honestEndorse("h2"))                          // journal: 3 -> 102, no coin to send
      .then(advEndorse("a", 3, 2))
      .then(btcBlock(HONEST))                             // BTC 3: only the adversary
      .then(btcBlock(HONEST))                             // BTC 4: nothing
      .then(processBtc("n1"))
      .then(processBtc("n1"))
      .then(processBtc("n1"))
      .then(processBtc("n1"))                             // B1 (2) ties A1 + A2 (2 + 0), smaller hash
      .then(bftPropose)                                   // 104 on B1: core 3, target 6
      .then(deliver("n1", 104))
      .then(btcBlock(Set()))                              // BTC 5: honest txs, delays 3 and 2
      .then(honestEndorse("h1"))                          // core 3 already reserved for 102
      .expect(and {
        // fork choice moved to B1; the fresh 104 ties with its parent and loses on hash (FL-05)
        canonicalTip(w.blocks, nodes.get("n1")) == 3,
        w.blocks.get(104).parent == 3,
        not(honestMintersNeverBlocked),
      })
      .then(restartMinter("h1"))                          // in-memory journal is gone
      .then(honestEndorse("h1"))                          // its A2 endorsement is confirmed now
      .then(btcBlock(Set()))                              // BTC 6
      .then(processBtc("n1"))
      .then(processBtc("n1"))
      .expect(not(honestNeverDoubleSigned))

  // FL-10: h1's endorsement of A1 waits in the mempool, so at the next tip h1 has no
  // confirmed coin to endorse A2. The core height stays reserved and A2 never gets h1's
  // endorsement, also after the coin confirms.
  run fl10HonestSendFailsTest =
    init
      .then(bftPropose)                                   // A1 = 101: core 2, target 2
      .then(deliver("n1", 101))
      .then(btcBlock(Set()))                              // BTC 1
      .then(honestEndorse("h1"))                          // spends h1's only coin
      .then(bftPropose)                                   // A2 = 102: core 3, target 3
      .then(deliver("n1", 102))
      .then(btcBlock(Set("h1")))                          // BTC 2 without it
      .then(honestEndorse("h1"))                          // reserved, not sent
      .expect(and {
        minters.get("h1").unsent == Set(3),
        not(honestEndorsementsSent),
      })
      .then(btcBlock(Set()))                              // BTC 3: the A1 endorsement, late
      .then(honestEndorse("h1"))                          // tip 3 -> target 4: nothing to do
      .then(btcBlock(Set()))                              // BTC 4
      .then(processBtc("n1"))
      .then(processBtc("n1"))
      .then(processBtc("n1"))
      .then(processBtc("n1"))
      .expect(and {
        not(nodes.get("n1").stored.keys().contains(("h1", 3))),
        nodes.get("n1").weights.get(102).effPower == 0,
        not(honestEndorsementsSent),
      })

  // FL-11: a is banned at BTC 2 but keeps its weight on P (only core height 3 is dropped,
  // FL-03). A Bitcoin reorg that removes only BTC 3, where a's later endorsement was ignored,
  // makes n1 re-evaluate P together with the double-sign evidence, and P loses a's weight.
  // n2 never saw BTC 3: same Bitcoin chain, same Flame blocks, different weights. No miner
  // censors anything: the fork includes the same endorsement again.
  run fl11RollbackDropsWeightTest =
    init
      .then(advBlock(GENESIS, true, 2, 1))                // P = 1: core 2, target 1
      .then(advBlock(1, true, 3, 2))                      // X = 2: core 3
      .then(advBlock(1, true, 3, 2))                      // Y = 3: core 3, other hash
      .then(advBlock(2, true, 4, 3))                      // Z = 4: core 4
      .then(deliver("n1", 1)).then(deliver("n1", 2)).then(deliver("n1", 3)).then(deliver("n1", 4))
      .then(deliver("n2", 1)).then(deliver("n2", 2)).then(deliver("n2", 3)).then(deliver("n2", 4))
      .then(advEndorse("a", 1, 2))
      .then(btcBlock(Set()))                              // BTC 1: a -> P, on time
      .then(advEndorse("a", 2, 3))
      .then(advEndorse("a", 3, 3))
      .then(btcBlock(Set()))                              // BTC 2: a double signs core 3
      .then(advEndorse("a", 4, 4))
      .then(mineBtc(Set()))                               // BTC 3 (hash 3): a -> Z, ignored
      .then(observeBtc("n1"))
      .then(processBtc("n1"))
      .then(processBtc("n1"))
      .expect(and {
        nodes.get("n1").doubleSigned == Set("a"),
        nodes.get("n1").weights.get(1).effPower == 4,
        not(rollbackUndoes),                              // already visible before the reorg
      })
      .then(processBtc("n1"))                             // n1 applies BTC 3
      .then(mineBtcOn(2, Set()))                          // hash 4 on BTC 2, a -> Z again
      .then(mineBtcOn(4, Set()))                          // hash 5: the fork is longer
      .then(observeBtc("n1"))                             // reorg of depth 1
      .then(observeBtc("n2"))
      .then(processBtc("n1"))                             // roll back hash 3
      .then(processBtc("n1"))                             // apply hash 4
      .then(processBtc("n1"))                             // apply hash 5
      .then(processBtc("n2"))
      .then(processBtc("n2"))
      .then(processBtc("n2"))
      .then(processBtc("n2"))
      .expect(and {
        nodes.get("n1").cursor == 5,
        nodes.get("n2").cursor == 5,
        nodes.get("n1").known == nodes.get("n2").known,
        nodes.get("n1").weights.get(1).effPower == 0,
        nodes.get("n2").weights.get(1).effPower == 4,
        not(determinism),
      })

  // FL-12: the honest BFT skipped slot 1 (its first target is tip + 2). The adversary fills it:
  // its branch has core blocks for slots 1 and 2, the honest one only for slot 2. Weight adds
  // up per core block, so log2(2) + log2(4) = 3 beats log2(7) = 2.81: 4 of 11 power displaces
  // an honest block every honest minter endorsed on time. Metadata is valid (not FL-01) and
  // exact weights do not help (not FL-05).
  run fl12SkippedSlotTest =
    init
      .then(bftPropose)                                   // 101: core 2, target 2
      .then(advBlock(GENESIS, true, 2, 1))                // 2: core 2, target 1, the skipped slot
      .then(advBlock(2, true, 3, 2))                      // 3: core 3, target 2
      .then(deliver("n1", 101))
      .then(deliver("n1", 2))
      .then(deliver("n1", 3))
      .then(btcBlock(Set()))                              // BTC 1
      .then(honestEndorse("h1"))                          // target 2 -> 101
      .then(honestEndorse("h2"))
      .then(advEndorse("a", 2, 2))
      .then(advEndorse("a", 3, 3))
      .then(btcBlock(Set()))                              // BTC 2: 101 on time, 2 one block late
      .then(processBtc("n1"))
      .then(processBtc("n1"))
      .expect(and {
        endorsedOnTimeByAllHonest(w.blocks, nodes.get("n1"), 101),
        canonicalTip(w.blocks, nodes.get("n1")) == 3,
        canonicalSlotsIncrease,
        not(honestCoreBlocksStay),
      })

  // FL-03: same blocks, same Bitcoin chain, different arrival order -> different weights.
  run fl03OrderDependenceTest =
    init
      .then(advBlock(GENESIS, true, 2, 1))                // B1 = 1: core 2
      .then(advBlock(GENESIS, true, 3, 2))                // X = 2: core 3
      .then(advBlock(GENESIS, true, 3, 2))                // Y = 3: core 3, other hash
      .then(advEndorse("a", 1, 2))
      .then(btcBlock(Set()))                               // BTC 1: a -> B1
      .then(advEndorse("a", 2, 3))
      .then(advEndorse("a", 3, 3))
      .then(btcBlock(Set()))                               // BTC 2: a signs X and Y
      // n1 knows B1 early
      .then(deliver("n1", 1))
      .then(processBtc("n1"))
      .then(deliver("n1", 2))
      .then(deliver("n1", 3))
      .then(processBtc("n1"))
      // n2 learns about the double sign before B1 arrives
      .then(processBtc("n2"))
      .then(deliver("n2", 2))
      .then(deliver("n2", 3))
      .then(processBtc("n2"))
      .then(deliver("n2", 1))
      .expect(and {
        nodes.get("n1").weights.get(1).effPower == 4,
        nodes.get("n2").weights.get(1).effPower == 0,
        not(determinism),
      })

  // FL-07: an identity without burns endorses a block that will never exist; it stays forever.
  run fl07PendingDosTest =
    init
      .then(advEndorse("x", GHOST, 1))
      .then(btcBlock(Set()))
      .then(processBtc("n1"))
      .then(btcBlock(Set()))
      .then(btcBlock(Set()))
      .then(btcBlock(Set()))
      .then(processBtc("n1"))
      .then(processBtc("n1"))
      .then(processBtc("n1"))
      .expect(and {
        nodes.get("n1").pending.size() == 1,
        not(pendingBounded),
      })

  // FL-08: endorse a hidden block on time, reveal it three BTC blocks later: full weight.
  run fl08HiddenForkTest =
    init
      .then(advBlock(GENESIS, true, 2, 1))                // id 1: core 2, withheld
      .then(advEndorse("a", 1, 2))
      .then(btcBlock(Set()))                               // BTC 1: on time
      .then(btcBlock(Set()))
      .then(btcBlock(Set()))
      .then(btcBlock(Set()))
      .then(processBtc("n1"))
      .then(processBtc("n1"))
      .then(processBtc("n1"))
      .then(processBtc("n1"))
      .then(deliver("n1", 1))                             // revealed at cursor 4
      .expect(and {
        nodes.get("n1").weights.get(1).effPower == 4,
        not(lateBlocksGetNoWeight),
      })

  // FL-09: the honest core block reaches n1 right after h1 processed the tip: h1 never
  // endorses it, h2 (which processes the tip later) does. The block gets 3 of 7 honest power.
  run fl09MissedSlotTest =
    init
      .then(bftPropose)                                   // id 101: core 2, target 2; gossip slow
      .then(btcBlock(Set()))                               // BTC 1
      .then(honestEndorse("h1"))                          // no core block for target 2 yet
      .then(deliver("n1", 101))                           // arrives at the same tip
      .then(honestEndorse("h2"))                          // h2 sees it
      .expect(and {
        minters.get("h1").missed == Set(2),
        not(honestMintersNeverMissSlot),
      })
      .then(btcBlock(Set()))                               // BTC 2: only h2, delay 0
      .then(honestEndorse("h1"))                          // tip 2 -> target 3: nothing
      .then(processBtc("n1"))
      .then(processBtc("n1"))
      .expect(and {
        nodes.get("n1").weights.get(101).effPower == 3,
        not(nodes.get("n1").stored.keys().contains(("h1", 2))),
        not(honestMintersNeverMissSlot),
      })

  // FL-06: parameters in code contradict the rationale.
  run fl06ParamsTest = init.expect(not(paramsMatchRationale))

  // Minor: 50 FLM in sparks reaches zero after 33 halvings, not 64 (design-doc.md:549).
  run halvingTest = init.expect(5_000_000_000 / 2^33 == 0 and 5_000_000_000 / 2^32 > 0)

  // ------------------------------------------------------------------
  // The normal scenario: the protocol works when nobody attacks, so the invariants
  // above are not violated (or satisfied) only because the model cannot move
  // ------------------------------------------------------------------

  // Two slots in a row: BFT proposes for target tip + 2, both honest minters endorse at
  // the next tip, every endorsement lands with delay 0, both nodes agree on the chain.
  run normalScenarioTest =
    init
      .then(bftPropose)                                   // 101: core 2, target 2
      .then(deliver("n1", 101))
      .then(deliver("n2", 101))
      .then(btcBlock(Set()))                              // BTC 1
      .then(bftPropose)                                   // 102 on 101: core 3, target 3
      .then(deliver("n1", 102))
      .then(deliver("n2", 102))
      .then(honestEndorse("h1"))                          // target 2 -> 101
      .then(honestEndorse("h2"))
      .then(btcBlock(Set()))                              // BTC 2: both on time
      .then(honestEndorse("h1"))                          // target 3 -> 102, change confirmed
      .then(honestEndorse("h2"))
      .then(btcBlock(Set()))                              // BTC 3: both on time
      .then(processBtc("n1"))
      .then(processBtc("n1"))
      .then(processBtc("n1"))
      .then(processBtc("n2"))
      .then(processBtc("n2"))
      .then(processBtc("n2"))
      .expect(and {
        NODES.forall(nd => and {
          canonicalTip(w.blocks, nodes.get(nd)) == 102,
          nodes.get(nd).weights.get(101).effPower == 7,
          nodes.get(nd).weights.get(102).effPower == 7,
          chainWeight(w.blocks, nodes.get(nd), 102) == 4,   // log2(7) + log2(7)
        }),
        endorsedOnTimeByAllHonest(w.blocks, nodes.get("n1"), 101),
        endorsedOnTimeByAllHonest(w.blocks, nodes.get("n1"), 102),
        allInvariants,
        bitcoinOk,
      })

  // Reachability witnesses: each must be VIOLATED by
  //   quint run flame_consensus_test.qnt --invariant <name>
  // otherwise random search cannot reach the states an invariant above talks about.
  val witnessNoBtcProcessed = NODES.forall(nd => nodes.get(nd).cursor == 0)
  val witnessNoEndorsementStored = NODES.forall(nd => nodes.get(nd).stored.keys() == Set())
  val witnessNoPending = NODES.forall(nd => nodes.get(nd).pending == Set())
  val witnessNoWeight = NODES.forall(nd =>
    nodes.get(nd).weights.keys().forall(b => nodes.get(nd).weights.get(b).effPower == 0))
  val witnessNoHonestEndorsementSent =
    w.payload.keys().forall(t => not(HONEST.contains(w.payload.get(t).minter)))
  // the premise of honestCoreBlocksStay
  val witnessNoBlockEndorsedOnTimeByAll = {
    val n = nodes.get("n1")
    n.known.forall(b => not(w.blocks.get(b).honest and w.blocks.get(b).isCore
      and endorsedOnTimeByAllHonest(w.blocks, n, b)))
  }
  val witnessNodesNeverAgree = {
    val a = nodes.get("n1")
    val b = nodes.get("n2")
    not(a.cursor == b.cursor and a.known == b.known and a.cursor != 0)
  }

  // ... and for the rollback properties
  val witnessNoBtcReorg = B_maxReorg == 0
  val witnessNoNodeOffBranch = NODES.forall(nd => onBtcBranch(nd))
  // the next block carries an endorsement of a minter the node already has state for
  val witnessNoRollbackOfKnownMinter = NODES.forall(nd => nextBlockMinters(nd).forall(m =>
    nodes.get(nd).stored.keys().forall(k => k._1 != m) and nodes.get(nd).pending.forall(e => e.minter != m)))
  val witnessNoRollbackOfBannedMinter = NODES.forall(nd => nextBlockMinters(nd).forall(m =>
    not(nodes.get(nd).doubleSigned.contains(m))))
}
