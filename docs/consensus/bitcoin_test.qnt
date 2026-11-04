// -*- mode: Bluespec; -*-
/**
 * Tests of bitcoin.qnt on one concrete instance.
 * Everything is a `run` test, so one command checks it all:
 *   quint test bitcoin_test.qnt
 * - unit tests of the library (A-F);
 * - invariantsHoldTest: random traces, all invariants hold in every state (G);
 * - *ReachableTest: concrete traces reach every witness state (H): the model can do
 *   what real Bitcoin does, so G does not hold vacuously;
 * - traces of revealing private transactions (I).
 * Deeper random search for G:  quint run bitcoin_test.qnt --invariant allInvariants
 */
/*
 * Unit tests of the pure library on hand-built forks.
 *
 *   W1: genesis funds alice (0,0) and bob (0,1), 10 each; relayed:
 *     tx1 alice: (0,0) -> [data, alice 10]
 *     tx2 alice: (0,0) -> [data, alice 10]      conflicts with tx1
 *     tx3 alice: (1,1) -> [data, alice 10]      child of tx1
 *     tx4 bob:   (0,1) -> [bob 10]
 *
 *   F (all published):
 *     0 ── 1{tx1} ── 2{tx3}                     branch X, height 2
 *     └── 3{}  ── 4{tx1} ── 5{}                 branch Y, height 3: tx1 re-included higher
 *   G = F + 6{tx2} on genesis                   branch Z: conflicting spend of alice's coin
 */
module bitcoin_test {
  pure val OBS = Set("o1", "o2")
  pure val OWN = Set("alice", "bob")
  pure val K = 2

  import bitcoin(
    OBSERVERS = OBS,
    OWNERS = OWN,
    K_REORG = K,
    CENSOR_WINDOW = 1,
    MAX_BLOCKS = 8,
    MAX_TXS = 4,
  ).* from "./bitcoin"


  pure val W0 = genesis([mkOut("alice", 10), mkOut("bob", 10)])
  pure val tx1 = mkTx(1, Set((0, 0)), [mkUnspendable("data", 0), mkOut("alice", 10)])
  pure val tx2 = mkTx(2, Set((0, 0)), [mkUnspendable("data", 0), mkOut("alice", 10)])
  pure val tx3 = mkTx(3, Set((1, 1)), [mkUnspendable("data", 0), mkOut("alice", 10)])
  pure val tx4 = mkTx(4, Set((0, 1)), [mkOut("bob", 10)])
  pure val W1 = W0.withTx(tx1).withTx(tx2).withTx(tx3).withTx(tx4)

  pure val F = W1
    .withBlock(0, Set(1), true, "honest")      // 1
    .withBlock(1, Set(3), true, "honest")      // 2
    .withBlock(0, Set(), true, "adversary")    // 3
    .withBlock(3, Set(1), true, "adversary")   // 4
    .withBlock(4, Set(), true, "adversary")    // 5
  pure val G = F.withBlock(0, Set(2), true, "adversary")  // 6


  run fixturesWellFormedTest = init.expect(and {
    treeWellFormed(F), treeWellFormed(G),
    allBodiesValid(F), allBodiesValid(G),
    noTxTwiceOnBranch(G), valueConserved(G), withheldOnlyAbovePublished(G),
  })

  // ---- A. Tree and branches ----

  run branchReachesGenesisTest = init.expect(and {
    chain(F, 5) == [0, 3, 4, 5],
    chain(F, 2) == [0, 1, 2],
    chain(F, 0) == [0],
    range(0, 4).foldl(true, (ok, k) => ok and heightOf(F, chain(F, 5)[k]) == k),
  })

  run forkBranchesShareOnlyPrefixTest = init.expect(and {
    branch(F, 2).intersect(branch(F, 5)) == Set(0),
    branch(F, 4).intersect(branch(F, 5)) == Set(0, 3, 4),
    isAncestor(F, 3, 5),
    not(isAncestor(F, 1, 5)),
  })

  run ancestorPropertiesTest = init.expect(and {
    ancestor(F, 2, 5) == 0,
    ancestor(F, 5, 2) == 0,                    // symmetric
    ancestor(F, 4, 5) == 4,                    // of a descendant
    ancestor(F, 5, 5) == 5,                    // of itself
    ancestor(G, 6, 2) == 0,
  })

  // ---- B. Transaction validity ----

  run inputMustExistTest = init.expect(not(validBody(W1, 0, Set(3))))           // parent not mined

  run noDoubleSpendInBlockTest = init.expect(not(validBody(W1, 0, Set(1, 2))))

  run noDoubleSpendOnBranchTest = init.expect(not(validBody(F, 1, Set(2))))

  run inBlockParentTest = init.expect(validBody(W1, 0, Set(1, 3)))

  run noDoubleInclusionTest = init.expect(and {
    not(validBody(F, 1, Set(1))),              // tx1 is already at height 1
    validBody(F, 3, Set(1)),                   // but not on branch Y yet
  })

  run badVoutTest = {
    val bad = mkTx(7, Set((0, 7)), [mkOut("alice", 1)])
    init.expect(and { not(wellFormed(W1, bad)), not(validBody(W1.withTx(bad), 0, Set(7))) })
  }

  run unspendableTest = {
    val spendData = mkTx(7, Set((1, 0)), [mkOut("alice", 0)])  // spends tx1's data output
    init.expect(and { not(wellFormed(F, spendData)), not(validBody(F.withTx(spendData), 1, Set(7))) })
  }

  run valueConservationTest = {
    val inflate = mkTx(7, Set((0, 1)), [mkOut("bob", 11)])
    val withFee = mkTx(8, Set((0, 1)), [mkOut("bob", 9)])
    init.expect(and {
      not(wellFormed(W1, inflate)),
      not(validBody(W1.withTx(inflate), 0, Set(7))),
      wellFormed(W1, withFee),
      validBody(W1.withTx(withFee), 0, Set(8)),
    })
  }

  run noSelfSpendTest = {
    val selfSpend = mkTx(7, Set((7, 0)), [mkOut("bob", 0)])
    init.expect(and { not(wellFormed(W1, selfSpend)), not(validBody(W1.withTx(selfSpend), 0, Set(7))) })
  }

  run noNewCoinbaseTest = {
    val mint = mkTx(7, Set(), [mkOut("bob", 50)])
    init.expect(and { not(wellFormed(W1, mint)), not(validBody(W1.withTx(mint), 0, Set(7))) })
  }

  run emptyOutputsTest = {
    val noOuts = mkTx(7, Set((0, 1)), [])
    init.expect(and { not(wellFormed(W1, noOuts)), not(validBody(W1.withTx(noOuts), 0, Set(7))) })
  }

  run negativeOutputTest = {
    val negative = mkTx(7, Set((0, 1)), [mkOut("bob", -100), mkOut("bob", 110)])
    init.expect(and { not(wellFormed(W1, negative)), not(validBody(W1.withTx(negative), 0, Set(7))) })
  }

  // two transactions spending each other would mint 10 from nothing
  run noSpendCycleTest = {
    val a = mkTx(7, Set((8, 0)), [mkOut("bob", 5)])
    val b = mkTx(8, Set((7, 0)), [mkOut("bob", 5)])
    init.expect(not(validBody(W1.withTx(a).withTx(b), 0, Set(7, 8))))
  }

  // ---- C. Reorgs ----

  run reincludedHigherTest = init.expect(and {
    inclHeight(F, 2, 1) == 1,
    inclHeight(F, 5, 1) == 2,                  // the reorg moved tx1 one block later
    inclHeight(F, 3, 1) == -1,                 // in between it was not on the branch
  })

  run plannerTest = init.expect(and {
    disconnects(F, 2, 5) == [2, 1],            // tip first
    connects(F, 2, 5) == [3, 4, 5],            // lowest first
    disconnects(F, 4, 5) == [],
    connects(F, 4, 5) == [5],                  // plain extension
    disconnects(F, 5, 5) == [] and connects(F, 5, 5) == [],
  })

  run droppedByConflictTest = init.expect(and {
    inclHeight(G, 6, 1) == -1,
    not(validBody(G, 6, Set(1))),
    not(mempool(G, 6).contains(1)),
  })

  run descendantsDroppedTest = init.expect(and {
    not(validBody(G, 6, Set(3))),
    not(validBody(G, 6, Set(1, 3))),
    not(mempool(G, 6).contains(3)),            // tx3 needs tx1, which lost
  })

  run reorgToLowerHeightTest = init.expect(and {
    disconnects(F, 5, 2) == [5, 4, 3],
    connects(F, 5, 2) == [1, 2],
    replay(F, 5, 2) == indexAt(F, 2),
  })

  run streamReplayTest = init.expect(and {
    replay(F, 2, 5) == indexAt(F, 5),
    replay(F, 5, 2) == indexAt(F, 2),
    replay(G, 2, 6) == indexAt(G, 6),
    replay(G, 6, 5) == indexAt(G, 5),
    replay(F, 2, 2) == indexAt(F, 2),
    streamOk(F, 2, 5), streamOk(G, 6, 5),
  })

  run streamEventsTest = init.expect(and {
    replay(F, 2, 5).incl.get(1) == 2,                                // tx1 re-indexed one block higher
    applyStream(F, indexAt(F, 2), [2, 1], [3]) == indexAt(F, 3),     // a page of one block
    not(applyStream(F, indexAt(F, 2), [1, 2], [3, 4, 5]).ok),        // disconnects tip last
    not(applyStream(F, indexAt(F, 2), [2, 1], [4, 3, 5]).ok),        // connects not lowest first
    not(applyStream(F, indexAt(F, 2), [2], [3, 4, 5]).ok),           // a stale block left behind
  })

  run reorgDepthTest = init.expect(and {
    reorgDepth(F, 2, 5) == 2,
    reorgDepth(F, 4, 5) == 0,
    reorgDepth(G, 5, 6) == 3,
  })

  // ---- D. Confirmations and wallet ----

  run confirmationsTest = init.expect(and {
    confirmations(F, 2, 1) == 2,
    confirmations(F, 2, 3) == 1,
    confirmations(F, 5, 3) == 0,
    confirmations(F, 0, GENESIS_TX) == 1,
  })

  run confirmationsAfterReorgTest = init.expect(and {
    confirmations(F, 1, 1) == 1,               // on X
    confirmations(F, 3, 1) == 0,               // Y before re-inclusion
    confirmations(F, 4, 1) == 1,
    confirmations(F, 5, 1) == 2,
  })

  run listUnspentTest = init.expect(and {
    listUnspent(F, 2, "alice", F.broadcast, 1) == Set((3, 1)),   // change of tx3, 1 confirmation
    listUnspent(F, 2, "alice", F.broadcast, 2) == Set(),
    listUnspent(F, 5, "alice", F.broadcast, 1) == Set(),          // tx3 pending on Y spends tx1's change
    listUnspent(F, 5, "alice", F.broadcast, 2) == Set(),
  })

  run singleUtxoStarvesTest = {
    val mined = F.withBlock(2, Set(4), true, "honest")                // 6: tx4 confirmed on X
    init.expect(and {
      utxoSet(F, 2).contains((0, 1)),                                // bob's coin is unspent on chain
      listUnspent(F, 2, "bob", F.broadcast, 1) == Set(),             // but tx4 is pending: nothing to spend
      listUnspent(mined, 6, "bob", mined.broadcast, 1) == Set((4, 0)), // its change after confirmation
    })
  }

  // a wallet that has not heard of tx4 offers bob's coin, which tx4 already spends
  run unseenSpendTest = init.expect(
    listUnspent(F, 2, "bob", F.broadcast.exclude(Set(4)), 1) == Set((0, 1)))

  // ---- E. Mempool ----

  run mempoolRelativeToTipTest = init.expect(and {
    mempool(W1, 0) == Set(1, 2, 3, 4),         // tx3 only as a package with tx1
    mempool(F, 2) == Set(4),                   // tx2 conflicts with tx1 on X
    mempool(F, 5) == Set(3, 4),
  })

  run reorgReturnsToMempoolTest = init.expect(and {
    mempool(F, 3) == Set(1, 2, 3, 4),          // tx1, tx3 left X and are eligible on Y
    validBody(F, 3, Set(1, 3)),
  })

  run conflictEvictedTest = init.expect(mempool(G, 6) == Set(4))

  run packageOrderTest = init.expect(and {
    not(validBody(W1, 0, Set(3))),
    validBody(W1, 0, Set(1, 3)),
    validBody(F, 1, Set(3)),
  })

  run privateTxTest = {
    val tx5 = mkTx(5, Set((0, 0)), [mkOut("alice", 10)])                     // alice's coin again
    val P = W1.withPrivateTx(tx5).withBlock(0, Set(5), false, "adversary")    // 1: withheld
    val Q = publishBranch(P, 1)
    init.expect(and {
      not(mempool(P, 0).contains(5)),                     // honest miners never see it
      eligible(P, 0, P.txs.keys()).contains(5),           // the adversary may mine it
      publishedRevealed(P),
      Q.broadcast.contains(5),                            // publishing reveals it
      mempool(Q, 0).contains(5),                          // so an orphaned block returns it
    })
  }

  // ---- F. Fork choice and observers ----

  run adoptsMoreWorkTest = init.expect(and {
    adopts(F, 2, 5),
    not(adopts(F, 5, 2)),
    bestTip(F) == 5,
  })

  run firstSeenTieTest = {
    val tie = W1.withBlock(0, Set(), true, "honest").withBlock(0, Set(), true, "honest")  // 1, 2
    init.expect(and {
      not(adopts(F, 2, 4)),                    // equal height: keep the first seen
      bestTip(tie) == 1,                       // the network breaks the tie by arrival
    })
  }

  // arrival is publication, not creation: 1 is older, but 2 was seen first
  run tieByPublicationOrderTest =
    init
      .then(mineAdversary(0, Set()))            // 1
      .then(mineAdversary(0, Set()))            // 2
      .then(publish(2))
      .then(publish(1))
      .expect(and { bestTip(btc) == 2, not(adopts(btc, 2, 1)) })

  run privateInvisibleTest = {
    val p = F.withBlock(5, Set(), false, "adversary").withBlock(6, Set(), false, "adversary")  // 6, 7
    val q = publishBranch(p, 7)
    init.expect(and {
      not(adopts(p, 5, 7)),
      bestTip(p) == 5,
      withheldOnlyAbovePublished(p),             // withheld blocks sit on top of published ones
      adopts(q, 5, 7),
      bestTip(q) == 7,
      q.blocks.get(6).published,               // publishing 7 relays 6 as well
    })
  }

  run observersMayDivergeTest = init.expect(and {
    adopts(F, 0, 1),
    adopts(F, 0, 3),
    not(adopts(F, 1, 3)),                      // same height: both stay where they are
    not(adopts(F, 3, 1)),
  })

  // ------------------------------------------------------------------
  // H. Reachability witnesses: each must be VIOLATED by `quint run`,
  //    otherwise the model forbids a behaviour real Bitcoin has
  // ------------------------------------------------------------------

  val witnessNoDeepReorg = maxReorg < K

  val witnessNoCrossBranchConflict =
    publishedBlocks(btc).forall(a => publishedBlocks(btc).forall(b =>
      btc.blocks.get(a).txs.forall(x => btc.blocks.get(b).txs.forall(y => not(conflicts(btc, x, y))))))

  val witnessNoShiftedInclusion =
    publishedBlocks(btc).forall(a => publishedBlocks(btc).forall(b =>
      onBranch(btc, a).intersect(onBranch(btc, b)).forall(t => inclHeight(btc, a, t) == inclHeight(btc, b, t))))

  val witnessObserversAgree = OBS.forall(o => OBS.forall(p => views.get(o) == views.get(p)))

  val witnessNoSameHeightSplit =
    OBS.forall(o => OBS.forall(p =>
      heightOf(btc, views.get(o)) == heightOf(btc, views.get(p)) implies views.get(o) == views.get(p)))

  // a transaction mined in a published block that left the best chain is back in its mempool
  val witnessNoReturnToMempool = {
    val b = bestTip(btc)
    mempool(btc, b).intersect(txsOf(btc, publishedBlocks(btc).exclude(branch(btc, b)))) == Set()
  }

  // an owner holds a coin on chain but has nothing confirmed and unspent to pay with
  val witnessNoStarvation = {
    val b = bestTip(btc)
    OWN.forall(o =>
      utxoSet(btc, b).exists(op => outAt(btc, op).owner == o) implies listUnspent(btc, b, o, btc.broadcast, 1) != Set())
  }

  // a block carries a transaction no node but its miner has seen
  val witnessNoPrivateInclusion = btc.blocks.keys().forall(h => btc.blocks.get(h).txs.subseteq(btc.broadcast))

  // honest miners compete at one height: a stale view, or a tie seen the other way
  val witnessNoHonestFork =
    btc.blocks.keys().forall(a => btc.blocks.keys().forall(b =>
      (a != b and btc.blocks.get(a).miner == HONEST and btc.blocks.get(b).miner == HONEST)
        implies heightOf(btc, a) != heightOf(btc, b)))

  // ------------------------------------------------------------------
  // G as a test: random runs, every state must satisfy all invariants.
  // `quint test --max-samples N` repeats it on N random traces.
  // ------------------------------------------------------------------

  // keeps a trace going when nothing else is enabled (all blocks mined, all observers synced)
  action stutter = all { btc' = btc, views' = views, maxReorg' = maxReorg, bcastHeight' = bcastHeight }

  run invariantsHoldTest =
    init.expect(allInvariants).then(30.reps(_ => any { step, stutter }.expect(allInvariants)))

  // ------------------------------------------------------------------
  // H as tests: a concrete trace reaches each witness state (the witness is violated)
  // ------------------------------------------------------------------

  run deepReorgReachableTest =
    init
      .then(mineHonest(0, Set()))               // 1
      .then(mineHonest(1, Set()))               // 2, height 2
      .then(observe("o1", 2))
      .then(mineAdversary(0, Set()))            // 3, private fork from genesis
      .then(mineAdversary(3, Set()))            // 4
      .then(mineAdversary(4, Set()))            // 5, height 3
      .then(publish(5))                         // reorgs the best chain by 2 = K
      .then(observe("o1", 5))
      .expect(not(witnessNoDeepReorg))

  run crossBranchConflictReachableTest =
    init
      .then(relay((0, 0), "alice"))             // tx1
      .then(relay((0, 0), "bob"))               // tx2 spends the same coin
      .then(mineHonest(0, Set(1)))              // 1
      .then(mineAdversary(0, Set(2)))           // 2
      .then(publish(2))
      .expect(not(witnessNoCrossBranchConflict))

  run shiftedInclusionReachableTest =
    init
      .then(relay((0, 0), "alice"))             // tx1
      .then(mineHonest(0, Set(1)))              // 1: tx1 at height 1
      .then(mineAdversary(0, Set()))            // 2
      .then(mineAdversary(2, Set(1)))           // 3: tx1 at height 2
      .then(publish(3))
      .expect(not(witnessNoShiftedInclusion))

  run observersDisagreeReachableTest =
    init
      .then(mineHonest(0, Set()))
      .then(observe("o1", 1))
      .expect(not(witnessObserversAgree))

  run sameHeightSplitReachableTest =
    init
      .then(mineHonest(0, Set()))               // 1
      .then(observe("o1", 1))
      .then(mineAdversary(0, Set()))            // 2, same height
      .then(publish(2))
      .then(observe("o2", 2))
      .expect(not(witnessNoSameHeightSplit))

  run returnToMempoolReachableTest =
    init
      .then(relay((0, 0), "alice"))             // tx1
      .then(mineHonest(0, Set(1)))              // 1
      .then(mineAdversary(0, Set()))            // 2
      .then(mineAdversary(2, Set()))            // 3
      .then(publish(3))                         // block 1 orphaned, tx1 back in the mempool
      .expect(not(witnessNoReturnToMempool))

  run starvationReachableTest =
    init
      .then(relay((0, 0), "alice"))             // the owner's only coin is spent by a pending tx
      .expect(not(witnessNoStarvation))

  run honestForkReachableTest =
    init
      .then(mineHonest(0, Set()))               // 1
      .then(mineHonest(0, Set()))               // 2: this miner has not seen 1 yet
      .expect(not(witnessNoHonestFork))

  // alice pays bob in public, then mines a private payment of the same coin to herself and
  // publishes it: the payment bob saw confirmed is gone, no node saw the double spend coming
  run privateDoubleSpendReachableTest =
    init
      .then(relay((0, 0), "bob"))               // tx1: alice pays bob
      .then(mineHonest(0, Set(1)))              // 1: bob sees a confirmation
      .then(withhold((0, 0), "alice"))          // tx2: the same coin back to alice
      .expect(orphan(btc, payment(btc, (2, 1), "bob")))   // a child of tx2: nodes would drop it
      .then(mineAdversary(0, Set(2)))           // 2
      .then(mineAdversary(2, Set()))            // 3
      .expect(and { not(witnessNoPrivateInclusion), not(btc.broadcast.contains(2)) })
      .then(publish(3))
      .expect(and { bestTip(btc) == 3, onBranch(btc, 3).contains(2), not(mempool(btc, 3).contains(1)) })

  // ------------------------------------------------------------------
  // I. The adversary relays a private transaction after all
  // ------------------------------------------------------------------

  run revealAfterBlocksTest =
    init
      .then(withhold((0, 0), "alice"))          // tx1, private
      .then(mineHonest(0, Set()))               // 1
      .then(mineHonest(1, Set()))               // 2
      .expect(not(mempool(btc, 2).contains(1)))
      .then(revealTx(1))
      .expect(and { mempool(btc, 2).contains(1), bcastHeight.get(1) == 2, allInvariants })

  run noSecondRevealTest =
    init
      .then(withhold((0, 0), "alice"))          // tx1
      .then(withhold((0, 1), "bob"))            // tx2
      .then(revealTx(1))
      .expect(not(revealable(btc, 1)))          // already relayed
      .then(mineAdversary(0, Set(2)))           // 1
      .then(publish(1))
      .expect(not(revealable(btc, 2)))          // revealed by its block

  run noOrphanRevealTest =
    init
      .then(withhold((0, 0), "alice"))          // tx1
      .then(withhold((1, 1), "alice"))          // tx2 spends tx1's change
      .expect(not(revealable(btc, 2)))          // nodes would drop it: tx1 is unknown
      .then(revealTx(1))
      .expect(revealable(btc, 2))
      .then(revealTx(2))
      .expect(mempool(btc, 0).intersect(Set(1, 2)) == Set(1, 2))
}
