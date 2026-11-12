// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "Dustpan",
    platforms: [.macOS(.v14)],
    targets: [
        .systemLibrary(name: "CDustpanCore"),
        .executableTarget(name: "Dustpan", dependencies: ["CDustpanCore"]),
    ]
)
