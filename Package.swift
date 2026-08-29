// swift-tools-version: 6.0

import PackageDescription

let package = Package(
  name: "greenbubbles",
  platforms: [
    .macOS(.v14)
  ],
  products: [
    .library(name: "GreenBubblesCore", targets: ["GreenBubblesCore"]),
    .library(name: "GreenBubblesHistory", targets: ["GreenBubblesHistory"]),
    .library(name: "GreenBubblesWeb", targets: ["GreenBubblesWeb"]),
    .library(name: "GreenBubblesAcquire", targets: ["GreenBubblesAcquire"]),
    .executable(name: "greenbubbles", targets: ["GreenBubblesCLI"]),
    .executable(
      name: "greenbubbles-public-article",
      targets: ["GreenBubblesArticleCLI"]
    ),
    .executable(
      name: "greenbubbles-acquire",
      targets: ["GreenBubblesAcquireCLI"]
    ),
    .executable(
      name: "greenbubbles-history",
      targets: ["GreenBubblesHistoryApp"]
    ),
  ],
  targets: [
    .target(name: "GreenBubblesCore"),
    .target(
      name: "GreenBubblesHistory",
      linkerSettings: [.linkedLibrary("sqlite3")]
    ),
    .target(name: "GreenBubblesWeb"),
    .target(
      name: "GreenBubblesAcquire",
      dependencies: ["GreenBubblesCore"]
    ),
    .executableTarget(
      name: "GreenBubblesCLI",
      dependencies: ["GreenBubblesCore"]
    ),
    .executableTarget(
      name: "GreenBubblesArticleCLI",
      dependencies: ["GreenBubblesWeb"]
    ),
    .executableTarget(
      name: "GreenBubblesAcquireCLI",
      dependencies: ["GreenBubblesAcquire"]
    ),
    .executableTarget(
      name: "GreenBubblesHistoryApp",
      dependencies: ["GreenBubblesHistory"],
      linkerSettings: [.linkedFramework("Security")]
    ),
    .testTarget(
      name: "GreenBubblesCoreTests",
      dependencies: ["GreenBubblesCore"]
    ),
    .testTarget(
      name: "GreenBubblesHistoryTests",
      dependencies: ["GreenBubblesHistory"]
    ),
    .testTarget(
      name: "GreenBubblesWebTests",
      dependencies: ["GreenBubblesWeb"]
    ),
    .testTarget(
      name: "GreenBubblesAcquireTests",
      dependencies: ["GreenBubblesAcquire"]
    ),
  ]
)
