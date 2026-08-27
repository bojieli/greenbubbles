// swift-tools-version: 6.0

import PackageDescription

let package = Package(
  name: "greenbubbles",
  platforms: [
    .macOS(.v14)
  ],
  products: [
    .library(name: "GreenBubblesCore", targets: ["GreenBubblesCore"]),
    .executable(name: "greenbubbles", targets: ["GreenBubblesCLI"]),
  ],
  targets: [
    .target(name: "GreenBubblesCore"),
    .executableTarget(
      name: "GreenBubblesCLI",
      dependencies: ["GreenBubblesCore"]
    ),
    .testTarget(
      name: "GreenBubblesCoreTests",
      dependencies: ["GreenBubblesCore"]
    ),
  ]
)
