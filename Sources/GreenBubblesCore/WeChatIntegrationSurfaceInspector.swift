import Darwin
import Foundation

public enum IntegrationSurfaceInspectionMode: String, Codable, Sendable {
  case staticSignedBundleMetadata
}

public enum IntegrationBundleComponentKind: String, Codable, Sendable {
  case mainApplication
  case helperApplication
  case appExtension
  case xpcService
}

public enum IntegrationEvidenceKind: String, Codable, Sendable {
  case urlScheme
  case extensionPoint
  case xpcService
  case applicationGroup
  case machLookupException
  case dataAccessAllowedProcess
  case bundledFramework
}

public enum IntegrationEvidenceClassification: String, Codable, Sendable {
  case inboundHandoff
  case systemManagedContentSurface
  case sharedContainerEntitlement
  case internalServiceReference
  case bundledImplementationDetail
}

public enum AuthenticatedReadEvidence: String, Codable, Sendable {
  case notProven
}

public struct IntegrationBundleComponent: Codable, Equatable, Sendable {
  public let relativeBundlePath: String
  public let bundleIdentifier: String
  public let kind: IntegrationBundleComponentKind
  public let packageType: String
  public let extensionPointIdentifier: String?
  public let xpcServiceType: String?
  public let fileProviderSupportsEnumeration: Bool?
  public let fileProviderDocumentGroup: String?
  public let sandboxed: Bool
  public let inheritsSandbox: Bool
  public let networkClient: Bool
  public let networkServer: Bool
  public let applicationGroups: [String]

  public init(
    relativeBundlePath: String,
    bundleIdentifier: String,
    kind: IntegrationBundleComponentKind,
    packageType: String,
    extensionPointIdentifier: String?,
    xpcServiceType: String?,
    fileProviderSupportsEnumeration: Bool?,
    fileProviderDocumentGroup: String?,
    sandboxed: Bool,
    inheritsSandbox: Bool,
    networkClient: Bool,
    networkServer: Bool,
    applicationGroups: [String]
  ) {
    self.relativeBundlePath = relativeBundlePath
    self.bundleIdentifier = bundleIdentifier
    self.kind = kind
    self.packageType = packageType
    self.extensionPointIdentifier = extensionPointIdentifier
    self.xpcServiceType = xpcServiceType
    self.fileProviderSupportsEnumeration = fileProviderSupportsEnumeration
    self.fileProviderDocumentGroup = fileProviderDocumentGroup
    self.sandboxed = sandboxed
    self.inheritsSandbox = inheritsSandbox
    self.networkClient = networkClient
    self.networkServer = networkServer
    self.applicationGroups = applicationGroups
  }
}

public struct IntegrationBoundaryEvidence: Codable, Equatable, Sendable {
  public let kind: IntegrationEvidenceKind
  public let identifier: String
  public let componentBundleIdentifier: String
  public let classification: IntegrationEvidenceClassification
  public let authenticatedReadEvidence: AuthenticatedReadEvidence

  public init(
    kind: IntegrationEvidenceKind,
    identifier: String,
    componentBundleIdentifier: String,
    classification: IntegrationEvidenceClassification,
    authenticatedReadEvidence: AuthenticatedReadEvidence = .notProven
  ) {
    self.kind = kind
    self.identifier = identifier
    self.componentBundleIdentifier = componentBundleIdentifier
    self.classification = classification
    self.authenticatedReadEvidence = authenticatedReadEvidence
  }
}

public enum ActiveReadFeasibilityState: String, Codable, Sendable {
  case unavailable
}

public struct ActiveReadFeasibilityAssessment: Codable, Equatable, Sendable {
  public let state: ActiveReadFeasibilityState
  public let reasonCode: String
  public let highLevelAuthenticatedReadContractProven: Bool
  public let messageReadAvailable: Bool
  public let momentsReadAvailable: Bool
  public let credentialExtractionPerformed: Bool
  public let liveProcessInteractionPerformed: Bool

  public init(
    state: ActiveReadFeasibilityState = .unavailable,
    reasonCode: String = "noAuthenticatedHighLevelReadContractProven",
    highLevelAuthenticatedReadContractProven: Bool = false,
    messageReadAvailable: Bool = false,
    momentsReadAvailable: Bool = false,
    credentialExtractionPerformed: Bool = false,
    liveProcessInteractionPerformed: Bool = false
  ) {
    self.state = state
    self.reasonCode = reasonCode
    self.highLevelAuthenticatedReadContractProven = highLevelAuthenticatedReadContractProven
    self.messageReadAvailable = messageReadAvailable
    self.momentsReadAvailable = momentsReadAvailable
    self.credentialExtractionPerformed = credentialExtractionPerformed
    self.liveProcessInteractionPerformed = liveProcessInteractionPerformed
  }
}

public struct WeChatIntegrationSurfaceReport: Codable, Equatable, Sendable {
  public let reportFormatVersion: Int
  public let generatedAt: Date
  public let inspectionMode: IntegrationSurfaceInspectionMode
  public let clientBuild: WeChatClientBuildFingerprint
  public let components: [IntegrationBundleComponent]
  public let boundaries: [IntegrationBoundaryEvidence]
  public let activeRead: ActiveReadFeasibilityAssessment

  public init(
    reportFormatVersion: Int = 1,
    generatedAt: Date,
    inspectionMode: IntegrationSurfaceInspectionMode = .staticSignedBundleMetadata,
    clientBuild: WeChatClientBuildFingerprint,
    components: [IntegrationBundleComponent],
    boundaries: [IntegrationBoundaryEvidence],
    activeRead: ActiveReadFeasibilityAssessment = ActiveReadFeasibilityAssessment()
  ) {
    self.reportFormatVersion = reportFormatVersion
    self.generatedAt = generatedAt
    self.inspectionMode = inspectionMode
    self.clientBuild = clientBuild
    self.components = components
    self.boundaries = boundaries
    self.activeRead = activeRead
  }
}

public enum IntegrationSurfaceInspectionError: Error, Equatable, CustomStringConvertible {
  case invalidApplicationBundle
  case unsupportedClientBuild
  case unsafeBundleComponent(String)
  case malformedMetadata(String)
  case metadataTooLarge(String)
  case commandFailed(String)
  case malformedEntitlements(String)
  case posix(operation: String, code: Int32)

  public var description: String {
    switch self {
    case .invalidApplicationBundle:
      return "The WeChat application bundle is missing or unsafe"
    case .unsupportedClientBuild:
      return "Static integration inspection is unavailable for this unpinned WeChat build"
    case .unsafeBundleComponent(let component):
      return "The pinned bundle component is missing or unsafe: \(component)"
    case .malformedMetadata(let component):
      return "The pinned bundle component has malformed metadata: \(component)"
    case .metadataTooLarge(let component):
      return "The pinned bundle component metadata exceeds the safety limit: \(component)"
    case .commandFailed(let command):
      return "Static integration inspection command failed: \(command)"
    case .malformedEntitlements(let component):
      return "The pinned bundle component has malformed signed entitlements: \(component)"
    case .posix(let operation, let code):
      return "\(operation) failed with POSIX error \(code)"
    }
  }
}

public struct WeChatIntegrationSurfaceInspector: Sendable {
  private static let maximumPlistBytes = 1_048_576
  private static let knownComponents: [(String, IntegrationBundleComponentKind)] = [
    (".", .mainApplication),
    ("Contents/MacOS/WeChatHelper.app", .helperApplication),
    ("Contents/MacOS/WeChatAppEx.app", .helperApplication),
    ("Contents/PlugIns/WeChatFileProviderExtension.appex", .appExtension),
    ("Contents/PlugIns/WeChatMacShare.appex", .appExtension),
    ("Contents/XPCServices/DebugHelper.xpc", .xpcService),
  ]

  private let homeDirectory: URL
  private let supportedBuilds: [WeChatClientBuildFingerprint]
  private let entitlementsProvider: @Sendable (URL, String) throws -> [String: Any]

  public init(homeDirectory: URL = FileManager.default.homeDirectoryForCurrentUser) {
    self.homeDirectory = homeDirectory.standardizedFileURL
    self.supportedBuilds = [Self.pinnedWeChat4113]
    self.entitlementsProvider = Self.readSignedEntitlements
  }

  init(
    homeDirectory: URL = FileManager.default.homeDirectoryForCurrentUser,
    supportedBuilds: [WeChatClientBuildFingerprint],
    entitlementsProvider: @escaping @Sendable (URL, String) throws -> [String: Any]
  ) {
    self.homeDirectory = homeDirectory.standardizedFileURL
    self.supportedBuilds = supportedBuilds
    self.entitlementsProvider = entitlementsProvider
  }

  public func inspectDefaultInstallation() throws -> WeChatIntegrationSurfaceReport? {
    let buildInspector = WeChatClientBuildInspector(homeDirectory: homeDirectory)
    guard let application = buildInspector.defaultApplicationURL() else { return nil }
    let build = try buildInspector.inspect(application: application)
    return try inspect(application: application, clientBuild: build)
  }

  public func inspect(
    application: URL,
    clientBuild: WeChatClientBuildFingerprint
  ) throws -> WeChatIntegrationSurfaceReport {
    guard supportedBuilds.contains(clientBuild), clientBuild.signatureValid else {
      throw IntegrationSurfaceInspectionError.unsupportedClientBuild
    }

    let application = application.standardizedFileURL
    try validateDirectory(application, label: ".")
    let mainInfo = try readInfoPlist(bundle: application, label: ".")
    guard
      string(mainInfo, "CFBundleIdentifier") == clientBuild.bundleIdentifier,
      string(mainInfo, "CFBundleShortVersionString") == clientBuild.marketingVersion,
      string(mainInfo, "CFBundleVersion") == clientBuild.buildVersion
    else { throw IntegrationSurfaceInspectionError.malformedMetadata(".") }

    var components: [IntegrationBundleComponent] = []
    var boundaries: [IntegrationBoundaryEvidence] = []

    for (relativePath, kind) in Self.knownComponents {
      let bundle =
        relativePath == "."
        ? application
        : application.appending(path: relativePath, directoryHint: .isDirectory).standardizedFileURL
      guard isWithin(bundle, root: application) else {
        throw IntegrationSurfaceInspectionError.unsafeBundleComponent(relativePath)
      }
      if relativePath != ".", !FileManager.default.fileExists(atPath: bundle.path) {
        continue
      }
      try validateDirectory(bundle, label: relativePath)
      let info =
        relativePath == "." ? mainInfo : try readInfoPlist(bundle: bundle, label: relativePath)
      let entitlements = try entitlementsProvider(bundle, relativePath)
      let component = try parseComponent(
        relativePath: relativePath,
        kind: kind,
        info: info,
        entitlements: entitlements
      )
      components.append(component)
      boundaries.append(
        contentsOf: try evidence(for: component, info: info, entitlements: entitlements))
    }

    let mainBundleIdentifier = clientBuild.bundleIdentifier
    for framework in try bundledFrameworkNames(application: application) {
      boundaries.append(
        IntegrationBoundaryEvidence(
          kind: .bundledFramework,
          identifier: framework,
          componentBundleIdentifier: mainBundleIdentifier,
          classification: .bundledImplementationDetail
        ))
    }

    return WeChatIntegrationSurfaceReport(
      generatedAt: Date(),
      clientBuild: clientBuild,
      components: components.sorted { $0.relativeBundlePath < $1.relativeBundlePath },
      boundaries: boundaries.sorted(by: Self.evidenceOrder)
    )
  }

  public static let pinnedWeChat4113 = WeChatClientBuildFingerprint(
    bundleIdentifier: "com.tencent.xinWeChat",
    marketingVersion: "4.1.13",
    buildVersion: "269579",
    executableSHA256: "041f2632f8c9f4208f0b1ad26d574384e0b854952097a851f7d9c7c6f64a8542",
    signingIdentifier: "com.tencent.xinWeChat",
    teamIdentifier: "5A4RE8SF68",
    codeDirectorySHA256: "c6b9f9587044784456eb96314f685c965fbd7d88bdacb72387284b8df551df4f",
    architectures: ["arm64", "x86_64"],
    hardenedRuntime: true,
    signatureValid: true
  )

  private func parseComponent(
    relativePath: String,
    kind: IntegrationBundleComponentKind,
    info: [String: Any],
    entitlements: [String: Any]
  ) throws -> IntegrationBundleComponent {
    guard
      let bundleIdentifier = string(info, "CFBundleIdentifier"),
      let packageType = string(info, "CFBundlePackageType"),
      !bundleIdentifier.isEmpty,
      !packageType.isEmpty
    else { throw IntegrationSurfaceInspectionError.malformedMetadata(relativePath) }

    let extensionDictionary = try optionalDictionary(info, "NSExtension", label: relativePath)
    let xpcDictionary = try optionalDictionary(info, "XPCService", label: relativePath)
    let groups = try stringArray(
      entitlements,
      "com.apple.security.application-groups",
      label: relativePath
    )

    return IntegrationBundleComponent(
      relativeBundlePath: relativePath,
      bundleIdentifier: bundleIdentifier,
      kind: kind,
      packageType: packageType,
      extensionPointIdentifier: string(extensionDictionary, "NSExtensionPointIdentifier"),
      xpcServiceType: string(xpcDictionary, "ServiceType"),
      fileProviderSupportsEnumeration: try optionalBool(
        extensionDictionary,
        "NSExtensionFileProviderSupportsEnumeration",
        label: relativePath
      ),
      fileProviderDocumentGroup: string(
        extensionDictionary,
        "NSExtensionFileProviderDocumentGroup"
      ),
      sandboxed: try bool(
        entitlements,
        "com.apple.security.app-sandbox",
        default: false,
        label: relativePath
      ),
      inheritsSandbox: try bool(
        entitlements,
        "com.apple.security.inherit",
        default: false,
        label: relativePath
      ),
      networkClient: try bool(
        entitlements,
        "com.apple.security.network.client",
        default: false,
        label: relativePath
      ),
      networkServer: try bool(
        entitlements,
        "com.apple.security.network.server",
        default: false,
        label: relativePath
      ),
      applicationGroups: groups.sorted()
    )
  }

  private func evidence(
    for component: IntegrationBundleComponent,
    info: [String: Any],
    entitlements: [String: Any]
  ) throws -> [IntegrationBoundaryEvidence] {
    var result: [IntegrationBoundaryEvidence] = []
    let bundleIdentifier = component.bundleIdentifier

    if component.kind == .mainApplication {
      for scheme in try urlSchemes(info, label: component.relativeBundlePath) {
        result.append(
          IntegrationBoundaryEvidence(
            kind: .urlScheme,
            identifier: scheme,
            componentBundleIdentifier: bundleIdentifier,
            classification: .inboundHandoff
          ))
      }

      for process in try dataAccessAllowedProcesses(info, label: component.relativeBundlePath) {
        result.append(
          IntegrationBoundaryEvidence(
            kind: .dataAccessAllowedProcess,
            identifier: process,
            componentBundleIdentifier: bundleIdentifier,
            classification: .internalServiceReference
          ))
      }
    }

    if let extensionPoint = component.extensionPointIdentifier {
      let classification: IntegrationEvidenceClassification =
        extensionPoint == "com.apple.share-services"
        ? .inboundHandoff
        : .systemManagedContentSurface
      result.append(
        IntegrationBoundaryEvidence(
          kind: .extensionPoint,
          identifier: extensionPoint,
          componentBundleIdentifier: bundleIdentifier,
          classification: classification
        ))
    }

    if let serviceType = component.xpcServiceType {
      result.append(
        IntegrationBoundaryEvidence(
          kind: .xpcService,
          identifier: serviceType,
          componentBundleIdentifier: bundleIdentifier,
          classification: .internalServiceReference
        ))
    }

    for group in component.applicationGroups {
      result.append(
        IntegrationBoundaryEvidence(
          kind: .applicationGroup,
          identifier: group,
          componentBundleIdentifier: bundleIdentifier,
          classification: .sharedContainerEntitlement
        ))
    }

    for service in try stringArray(
      entitlements,
      "com.apple.security.temporary-exception.mach-lookup.global-name",
      label: component.relativeBundlePath
    ) {
      result.append(
        IntegrationBoundaryEvidence(
          kind: .machLookupException,
          identifier: service,
          componentBundleIdentifier: bundleIdentifier,
          classification: .internalServiceReference
        ))
    }

    return result
  }

  private func bundledFrameworkNames(application: URL) throws -> [String] {
    let frameworks = application.appending(path: "Contents/Frameworks", directoryHint: .isDirectory)
    guard FileManager.default.fileExists(atPath: frameworks.path) else { return [] }
    try validateDirectory(frameworks, label: "Contents/Frameworks")
    let children = try FileManager.default.contentsOfDirectory(
      at: frameworks,
      includingPropertiesForKeys: [.isDirectoryKey, .isSymbolicLinkKey],
      options: [.skipsHiddenFiles]
    )
    guard children.count <= 256 else {
      throw IntegrationSurfaceInspectionError.malformedMetadata("Contents/Frameworks")
    }
    var names: [String] = []
    for child in children where child.pathExtension == "framework" {
      let values = try child.resourceValues(forKeys: [.isDirectoryKey, .isSymbolicLinkKey])
      guard values.isDirectory == true, values.isSymbolicLink != true else {
        throw IntegrationSurfaceInspectionError.unsafeBundleComponent("Contents/Frameworks")
      }
      names.append(child.deletingPathExtension().lastPathComponent)
    }
    return names.sorted()
  }

  private func readInfoPlist(bundle: URL, label: String) throws -> [String: Any] {
    let info = bundle.appending(path: "Contents/Info.plist").standardizedFileURL
    guard isWithin(info, root: bundle) else {
      throw IntegrationSurfaceInspectionError.unsafeBundleComponent(label)
    }
    let data = try readRegularFile(info, label: label)
    guard
      let value = try? PropertyListSerialization.propertyList(from: data, format: nil),
      let dictionary = value as? [String: Any]
    else { throw IntegrationSurfaceInspectionError.malformedMetadata(label) }
    return dictionary
  }

  private func readRegularFile(_ url: URL, label: String) throws -> Data {
    let descriptor = Darwin.open(url.path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW)
    guard descriptor >= 0 else {
      throw IntegrationSurfaceInspectionError.posix(operation: "open bundle metadata", code: errno)
    }
    defer { Darwin.close(descriptor) }
    var metadata = stat()
    guard Darwin.fstat(descriptor, &metadata) == 0 else {
      throw IntegrationSurfaceInspectionError.posix(
        operation: "inspect bundle metadata", code: errno)
    }
    guard metadata.st_mode & S_IFMT == S_IFREG, metadata.st_nlink == 1 else {
      throw IntegrationSurfaceInspectionError.unsafeBundleComponent(label)
    }
    guard metadata.st_size >= 0, metadata.st_size <= Self.maximumPlistBytes else {
      throw IntegrationSurfaceInspectionError.metadataTooLarge(label)
    }
    let handle = FileHandle(fileDescriptor: descriptor, closeOnDealloc: false)
    return try handle.readToEnd() ?? Data()
  }

  private func validateDirectory(_ url: URL, label: String) throws {
    var metadata = stat()
    guard Darwin.lstat(url.path, &metadata) == 0 else {
      throw IntegrationSurfaceInspectionError.posix(
        operation: "inspect application bundle", code: errno)
    }
    guard metadata.st_mode & S_IFMT == S_IFDIR else {
      throw IntegrationSurfaceInspectionError.unsafeBundleComponent(label)
    }
  }

  private func isWithin(_ url: URL, root: URL) -> Bool {
    url.path == root.path || url.path.hasPrefix(root.path + "/")
  }

  private func string(_ dictionary: [String: Any]?, _ key: String) -> String? {
    dictionary?[key] as? String
  }

  private func optionalDictionary(
    _ dictionary: [String: Any]?,
    _ key: String,
    label: String
  ) throws -> [String: Any]? {
    guard let value = dictionary?[key] else { return nil }
    guard let result = value as? [String: Any] else {
      throw IntegrationSurfaceInspectionError.malformedMetadata(label)
    }
    return result
  }

  private func bool(
    _ dictionary: [String: Any],
    _ key: String,
    default defaultValue: Bool,
    label: String
  ) throws -> Bool {
    guard dictionary[key] != nil else { return defaultValue }
    guard let value = dictionary[key] as? Bool else {
      throw IntegrationSurfaceInspectionError.malformedEntitlements(label)
    }
    return value
  }

  private func optionalBool(
    _ dictionary: [String: Any]?,
    _ key: String,
    label: String
  ) throws -> Bool? {
    guard let value = dictionary?[key] else { return nil }
    guard let result = value as? Bool else {
      throw IntegrationSurfaceInspectionError.malformedMetadata(label)
    }
    return result
  }

  private func stringArray(
    _ dictionary: [String: Any],
    _ key: String,
    label: String
  ) throws -> [String] {
    guard let value = dictionary[key] else { return [] }
    guard let array = value as? [Any] else {
      throw IntegrationSurfaceInspectionError.malformedEntitlements(label)
    }
    let strings = array.compactMap { $0 as? String }
    guard strings.count == array.count, strings.allSatisfy({ !$0.isEmpty }) else {
      throw IntegrationSurfaceInspectionError.malformedEntitlements(label)
    }
    return Array(Set(strings)).sorted()
  }

  private func urlSchemes(_ info: [String: Any], label: String) throws -> [String] {
    guard let value = info["CFBundleURLTypes"] else { return [] }
    guard let types = value as? [[String: Any]] else {
      throw IntegrationSurfaceInspectionError.malformedMetadata(label)
    }
    var schemes: [String] = []
    for type in types {
      guard let rawSchemes = type["CFBundleURLSchemes"] as? [Any] else {
        throw IntegrationSurfaceInspectionError.malformedMetadata(label)
      }
      let strings = rawSchemes.compactMap { $0 as? String }
      guard strings.count == rawSchemes.count, strings.allSatisfy({ !$0.isEmpty }) else {
        throw IntegrationSurfaceInspectionError.malformedMetadata(label)
      }
      schemes.append(contentsOf: strings)
    }
    return Array(Set(schemes)).sorted()
  }

  private func dataAccessAllowedProcesses(
    _ info: [String: Any],
    label: String
  ) throws -> [String] {
    guard let policy = try optionalDictionary(info, "NSDataAccessSecurityPolicy", label: label)
    else { return [] }
    guard let rawProcesses = policy["AllowProcesses"] else { return [] }
    guard let processes = rawProcesses as? [String: Any] else {
      throw IntegrationSurfaceInspectionError.malformedMetadata(label)
    }
    var identifiers: [String] = []
    for value in processes.values {
      guard let rawIdentifiers = value as? [Any] else {
        throw IntegrationSurfaceInspectionError.malformedMetadata(label)
      }
      let strings = rawIdentifiers.compactMap { $0 as? String }
      guard strings.count == rawIdentifiers.count, strings.allSatisfy({ !$0.isEmpty }) else {
        throw IntegrationSurfaceInspectionError.malformedMetadata(label)
      }
      identifiers.append(contentsOf: strings)
    }
    return Array(Set(identifiers)).sorted()
  }

  private static func evidenceOrder(
    _ lhs: IntegrationBoundaryEvidence,
    _ rhs: IntegrationBoundaryEvidence
  ) -> Bool {
    let left = "\(lhs.kind.rawValue)\u{0}\(lhs.identifier)\u{0}\(lhs.componentBundleIdentifier)"
    let right = "\(rhs.kind.rawValue)\u{0}\(rhs.identifier)\u{0}\(rhs.componentBundleIdentifier)"
    return left < right
  }

  private static func readSignedEntitlements(
    _ bundle: URL,
    label: String
  ) throws -> [String: Any] {
    let process = Process()
    let output = Pipe()
    process.executableURL = URL(fileURLWithPath: "/usr/bin/codesign")
    process.arguments = ["-d", "--entitlements", ":-", bundle.path]
    process.standardOutput = output
    process.standardError = output
    do {
      try process.run()
    } catch {
      throw IntegrationSurfaceInspectionError.commandFailed("codesign")
    }
    process.waitUntilExit()
    let data = output.fileHandleForReading.readDataToEndOfFile()
    guard process.terminationReason == .exit, process.terminationStatus == 0 else {
      throw IntegrationSurfaceInspectionError.commandFailed("codesign")
    }
    guard let text = String(data: data, encoding: .utf8) else {
      throw IntegrationSurfaceInspectionError.malformedEntitlements(label)
    }
    guard let start = text.range(of: "<?xml"), let end = text.range(of: "</plist>") else {
      return [:]
    }
    let xml = Data(text[start.lowerBound..<end.upperBound].utf8)
    guard
      xml.count <= maximumPlistBytes,
      let value = try? PropertyListSerialization.propertyList(from: xml, format: nil),
      let dictionary = value as? [String: Any]
    else { throw IntegrationSurfaceInspectionError.malformedEntitlements(label) }
    return dictionary
  }
}
