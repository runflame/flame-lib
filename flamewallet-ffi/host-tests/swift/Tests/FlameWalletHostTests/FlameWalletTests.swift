import FlameWallet
import Foundation
import XCTest

/// What the Rust tests cannot see: that the Swift UniFFI generates loads the
/// library, passes records both ways, and turns errors into `FlameError`.
final class FlameWalletTests: XCTestCase {
    func testMnemonicAndAddresses() throws {
        let phrase = try generateMnemonic(wordCount: 12)
        XCTAssertTrue(validateMnemonic(phrase: phrase))

        let seed = try mnemonicToSeed(phrase: phrase, passphrase: "")
        XCTAssertEqual(seed.count, 64)
        let wallet = try Wallet(seed: seed, network: .testnet, nextIndex: 0)
        let first = try wallet.nextAddress()
        XCTAssertEqual(first.path, KeyPath(branch: 0, index: 0))
        XCTAssertTrue(first.address.hasPrefix("tf1"))
        XCTAssertEqual(wallet.nextIndex(), 1)

        let predicate = try addressToPredicate(address: first.address, network: .testnet)
        XCTAssertEqual(predicate, first.predicate)
        XCTAssertEqual(try wallet.owns(predicate: predicate, gap: 0), KeyPath(branch: 0, index: 0))
        XCTAssertTrue(wallet.receivingKey().hasPrefix("testrecv1"))
    }

    func testErrorsNameTheField() {
        XCTAssertThrowsError(try decodeContract(bytes: Data([1, 2, 3]))) { error in
            guard case FlameError.InvalidBytes(let what, _) = error else {
                return XCTFail("expected InvalidBytes, got \(error)")
            }
            XCTAssertEqual(what, "contract")
        }
        XCTAssertThrowsError(try Wallet(seed: Data(count: 63), network: .testnet, nextIndex: 0)) { error in
            guard case FlameError.InvalidSeed = error else {
                return XCTFail("expected InvalidSeed, got \(error)")
            }
        }
    }
}
