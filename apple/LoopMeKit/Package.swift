// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "LoopMeKit",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [
        .library(name: "LoopMeKit", targets: ["LoopMeKit"]),
    ],
    targets: [
        .target(name: "LoopMeKit"),
        .testTarget(name: "LoopMeKitTests", dependencies: ["LoopMeKit"]),
    ]
)
