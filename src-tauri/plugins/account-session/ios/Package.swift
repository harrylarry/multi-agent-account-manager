// swift-tools-version:5.9
// The swift-tools-version declares the minimum version of Swift required to build this package.

import PackageDescription

let package = Package(
  name: "tauri-plugin-account-session",
  platforms: [
    .iOS(.v17),
  ],
  products: [
    .library(
      name: "tauri-plugin-account-session",
      type: .static,
      targets: ["tauri-plugin-account-session"])
  ],
  dependencies: [
    .package(name: "Tauri", path: "../.tauri/tauri-api")
  ],
  targets: [
    .target(
      name: "tauri-plugin-account-session",
      dependencies: [
        .byName(name: "Tauri")
      ],
      path: "Sources")
  ]
)
