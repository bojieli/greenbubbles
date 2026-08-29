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
    .library(name: "GreenBubblesSendKit", targets: ["GreenBubblesSendKit"]),
    .executable(name: "greenbubbles-discover", targets: ["GreenBubblesCLI"]),
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
    .executable(
      name: "greenbubbles-send",
      targets: ["GreenBubblesSendCLI"]
    ),
    .executable(
      name: "greenbubbles-input-helper",
      targets: ["GreenBubblesInputHelper"]
    ),
  ],
  targets: [
    .target(name: "GreenBubblesCore"),
    .target(
      name: "GreenBubblesHistory",
      linkerSettings: [.linkedLibrary("sqlite3")]
    ),
    .target(name: "GreenBubblesWeb"),
    .target(name: "GreenBubblesSendKit"),
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
    .executableTarget(
      name: "GreenBubblesSendCLI",
      dependencies: ["GreenBubblesSendKit"],
      linkerSettings: [.linkedFramework("AppKit")]
    ),
    .executableTarget(
      name: "GreenBubblesInputHelper",
      dependencies: ["GreenBubblesSendKit"],
      linkerSettings: [
        .linkedFramework("AppKit"),
        .linkedFramework("ScreenCaptureKit"),
        .linkedFramework("Vision"),
      ]
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
    .testTarget(
      name: "GreenBubblesSendKitTests",
      dependencies: ["GreenBubblesSendKit"]
    ),
  ]
)
