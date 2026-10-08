// The package exercised the way JavaScript will use it: through the built
// glue, with test/fixtures.json (tests/fixtures.rs) as a real chain's bytes.
//
//   flamewallet-wasm/scripts/build.sh && node --test flamewallet-wasm/test/*.test.mjs

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath, pathToFileURL } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const pkg = resolve(process.env.FLAME_WASM_PKG ?? resolve(here, '../../target/wallet-wasm/pkg'));
const wasm = await import(pathToFileURL(resolve(pkg, 'flamewallet_wasm.js')).href);
wasm.initSync({ module: readFileSync(resolve(pkg, 'flamewallet_wasm_bg.wasm')) });

const fixtures = JSON.parse(readFileSync(resolve(here, 'fixtures.json'), 'utf8'));
const bytes = hex => Uint8Array.from(hex.match(/../g) ?? [], byte => parseInt(byte, 16));
const clear = fixtures.scenarios.clear;
const confidential = fixtures.scenarios.confidential;

function request(json) {
    return {
        inputs: json.inputs.map(input => ({
            contract: bytes(input.contract),
            proof: bytes(input.proof),
            path: input.path,
            opening: input.opening && {
                qty: BigInt(input.opening.qty),
                flavor: bytes(input.opening.flavor),
                qtyBlinding: bytes(input.opening.qtyBlinding),
                flavorBlinding: bytes(input.opening.flavorBlinding)
            }
        })),
        outputs: json.outputs.map(output => ({
            address: output.address,
            qty: BigInt(output.qty)
        })),
        fee: BigInt(json.fee),
        gas: BigInt(json.gas)
    };
}

function flameError(kind, fields = {}) {
    return error => {
        assert.equal(error.name, 'FlameError');
        assert.equal(error.kind, kind);
        for (const [name, value] of Object.entries(fields)) {
            assert.equal(error[name], value);
        }
        return true;
    };
}

test('mnemonics', () => {
    const phrase = wasm.generateMnemonic(24);
    assert.equal(phrase.split(' ').length, 24);
    assert.ok(wasm.validateMnemonic(phrase));
    assert.ok(!wasm.validateMnemonic('abandon abandon'));
    assert.throws(() => wasm.generateMnemonic(13), flameError('invalidMnemonic'));

    const seed = wasm.mnemonicToSeed(phrase, 'pass');
    assert.ok(seed instanceof Uint8Array);
    assert.equal(seed.length, 64);
    const a = wasm.Wallet.fromSeed(seed, 'mainnet', 0);
    const b = wasm.Wallet.fromMnemonic(phrase, 'pass', 'mainnet', 0);
    assert.deepEqual(a.address({ branch: 0, index: 3 }), b.address({ branch: 0, index: 3 }));
    assert.equal(a.network(), 'mainnet');
});

test('addresses and ownership', () => {
    const wallet = wasm.Wallet.fromSeed(bytes(clear.seed), 'testnet', 0);
    const first = wallet.nextAddress();
    assert.deepEqual(first.path, { branch: 0, index: 0 });
    assert.ok(first.address.startsWith('tf1'));
    assert.equal(wallet.nextIndex(), 1);
    assert.deepEqual(wasm.addressToPredicate(first.address, 'testnet'), first.predicate);
    assert.throws(() => wasm.addressToPredicate(first.address, 'mainnet'), flameError('invalidAddress'));
    assert.deepEqual(wallet.owns(first.predicate, 0), { branch: 0, index: 0 });
    assert.equal(wallet.owns(new Uint8Array(32).fill(1), 5), undefined);
    assert.ok(wallet.receivingKey().startsWith('testrecv1'));
    assert.ok(wallet.viewKey().startsWith('testview1'));
});

test('a transfer from served bytes to signed bytes', () => {
    const alice = wasm.Wallet.fromSeed(bytes(clear.seed), 'testnet', 1);
    const bob = wasm.Wallet.fromSeed(bytes(confidential.seed), 'testnet', 1);

    const allocation = wasm.decodeContract(bytes(clear.request.inputs[0].contract));
    assert.equal(allocation.value.kind, 'clear');
    assert.equal(typeof allocation.value.qty, 'bigint');
    assert.deepEqual(alice.owns(allocation.predicate, 0), { branch: 0, index: 0 });

    const transfer = alice.buildTransfer(request(clear.request));
    assert.equal(transfer.txid.length, 32);
    assert.ok(transfer.blockTx.length > 0);
    assert.equal(transfer.outputs.length, 2);

    // Outputs are sorted, so each is found by whose predicate locks it.
    const bobPath = { branch: 0, index: 0 };
    const changePath = { branch: 1, index: 0 };
    const toBob = transfer.outputs.find(output =>
        bob.owns(wasm.decodeContract(output.contract).predicate, 0) !== undefined);
    const toChange = transfer.outputs.find(output => output !== toBob);
    const info = wasm.decodeContract(toBob.contract);
    assert.deepEqual(info.value, { kind: 'confidential' });
    assert.deepEqual(info.id, toBob.contractId);
    assert.deepEqual(bob.owns(info.predicate, 0), bobPath);
    assert.deepEqual(alice.owns(wasm.decodeContract(toChange.contract).predicate, 0), changePath);

    const received = bob.openNote(toBob.contract, toBob.note, bobPath);
    assert.equal(received.opening.qty, BigInt(clear.request.outputs[0].qty));
    assert.ok(received.memo instanceof Uint8Array);
    assert.ok(wasm.openingMatches(toBob.contract, received.opening));
    const kept = alice.openNote(toChange.contract, toChange.note, changePath);
    assert.equal(kept.opening.qty, BigInt(clear.request.outputs[1].qty));
    assert.ok(!wasm.openingMatches(toBob.contract, kept.opening));

    assert.throws(() => bob.openNote(toBob.contract, undefined, bobPath),
        flameError('note', { failure: 'missing' }));
    assert.throws(() => bob.openNote(toBob.contract, toChange.note, bobPath),
        flameError('note', { failure: 'undecryptable' }));

    // Bob from his view key alone opens the same note and cannot spend;
    // from his receiving key alone he finds it and can neither open nor spend.
    const view = wasm.Wallet.fromViewKey(bob.viewKey(), 'testnet', bob.nextIndex());
    const receive = wasm.Wallet.fromReceivingKey(bob.receivingKey(), 'testnet', bob.nextIndex());
    assert.equal(bob.kind(), 'spend');
    assert.equal(view.kind(), 'view');
    assert.equal(receive.kind(), 'receive');
    for (const wallet of [view, receive]) {
        assert.equal(wallet.receivingKey(), bob.receivingKey());
        assert.deepEqual(wallet.owns(info.predicate, 0), bobPath);
    }
    assert.deepEqual(view.openNote(toBob.contract, toBob.note, bobPath), received);
    const spend = request(confidential.request);
    assert.throws(() => view.buildTransfer(spend),
        flameError('notPermitted', { wallet: 'view', needs: 'spend' }));
    assert.throws(() => receive.buildTransfer(spend),
        flameError('notPermitted', { wallet: 'receive', needs: 'spend' }));
    assert.throws(() => receive.openNote(toBob.contract, toBob.note, bobPath),
        flameError('notPermitted', { wallet: 'receive', needs: 'view' }));
    assert.throws(() => receive.viewKey(),
        flameError('notPermitted', { wallet: 'receive', needs: 'view' }));
    assert.throws(() => wasm.Wallet.fromViewKey(bob.viewKey(), 'mainnet', 0), flameError('invalidKey'));
    assert.throws(() => wasm.Wallet.fromViewKey(bob.receivingKey(), 'testnet', 0), flameError('invalidKey'));
    assert.throws(() => wasm.Wallet.fromReceivingKey(bob.viewKey(), 'testnet', 0), flameError('invalidKey'));
});

test('spending a confidential input with its opening', () => {
    const bob = wasm.Wallet.fromSeed(bytes(confidential.seed), 'testnet', 1);
    const spend = request(confidential.request);
    const received = wasm.decodeContract(spend.inputs[0].contract);
    assert.deepEqual(received.value, { kind: 'confidential' });
    // The opening the input carries is the one its note gives.
    const input = confidential.request.inputs[0];
    const opened = bob.openNote(spend.inputs[0].contract, bytes(input.note), input.path);
    assert.deepEqual(opened.opening, spend.inputs[0].opening);
    assert.ok(wasm.openingMatches(spend.inputs[0].contract, spend.inputs[0].opening));

    const transfer = bob.buildTransfer(spend);
    assert.equal(transfer.outputs.length, 2);
    const changePath = { branch: 1, index: 0 };
    const toChange = transfer.outputs.find(output =>
        bob.owns(wasm.decodeContract(output.contract).predicate, 0) !== undefined);
    assert.equal(
        bob.openNote(toChange.contract, toChange.note, changePath).opening.qty,
        spend.outputs[1].qty
    );

    // Without its opening a confidential input cannot be spent.
    const blind = request(confidential.request);
    delete blind.inputs[0].opening;
    assert.throws(() => bob.buildTransfer(blind), flameError('transfer'));
    bob.free();
});

test('errors name what was wrong', () => {
    const alice = wasm.Wallet.fromSeed(bytes(clear.seed), 'testnet', 1);
    assert.throws(() => wasm.decodeContract(new Uint8Array([1, 2, 3])), flameError('invalidBytes', { what: 'contract' }));
    assert.throws(() => wasm.Wallet.fromSeed(new Uint8Array(63), 'testnet', 0), flameError('invalidSeed'));

    const wrongKey = request(clear.request);
    wrongKey.inputs[0].path = { branch: 0, index: 1 };
    assert.throws(() => alice.buildTransfer(wrongKey), flameError('keyMismatch', { input: 0 }));

    const wrongNetwork = request(clear.request);
    wrongNetwork.outputs[0].address = wasm.Wallet.fromSeed(bytes(clear.seed), 'mainnet', 0).nextAddress().address;
    assert.throws(() => alice.buildTransfer(wrongNetwork), flameError('invalidAddress'));

    // A shape mistake is the caller's bug, thrown as a TypeError.
    const misshapen = request(clear.request);
    misshapen.fee = 'a thousand';
    assert.throws(() => alice.buildTransfer(misshapen), TypeError);
    assert.throws(() => alice.address({ branch: 0 }), TypeError);
});
