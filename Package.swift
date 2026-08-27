// swift-tools-version: 6.0

import PackageDescription

let package = Package(
  name: "greenbubbles",
  platforms: [
    .macOS(.v14)
  ],
  products: [
    .library(name: "GreenBubblesCore", targets: ["GreenBubblesCore"]),
    .library(name: "GreenBubblesWeb", targets: ["GreenBubblesWeb"]),
    .executable(name: "greenbubbles", targets: ["GreenBubblesCLI"]),
    .executable(
      name: "greenbubbles-public-article",
      targets: ["GreenBubblesArticleCLI"]
    ),
  ],
  targets: [
    .target(name: "GreenBubblesCore"),
    .target(name: "GreenBubblesWeb"),
    .executableTarget(
      name: "GreenBubblesCLI",
      dependencies: ["GreenBubblesCore"]
    ),
    .executableTarget(
      name: "GreenBubblesArticleCLI",
      dependencies: ["GreenBubblesWeb"]
    ),
    .testTarget(
      name: "GreenBubblesCoreTests",
      dependencies: ["GreenBubblesCore"]
    ),
    .testTarget(
      name: "GreenBubblesWebTests",
      dependencies: ["GreenBubblesWeb"]
    ),
  ]
)
