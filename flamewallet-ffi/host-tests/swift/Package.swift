// swift-tools-version:5.9
// The generated Swift, run on a Mac against the xcframework's macOS slice.
// Build the package first; CI does the same:
//
//   flamewallet-ffi/scripts/build-apple.sh
//   cd flamewallet-ffi/host-tests/swift && swift test
import PackageDescription

let package = Package(
    name: "FlameWalletHostTests",
    platforms: [.macOS(.v11)],
    dependencies: [
        .package(path: "../../../target/flamewallet-ffi/apple/FlameWallet"),
    ],
    targets: [
        .testTarget(
            name: "FlameWalletHostTests",
            dependencies: [.product(name: "FlameWallet", package: "FlameWallet")]
        ),
    ]
)
