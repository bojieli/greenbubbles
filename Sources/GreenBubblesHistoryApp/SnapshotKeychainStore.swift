import Darwin
import Foundation
import Security

enum SnapshotKeychainError: Error, LocalizedError {
  case invalidSnapshot
  case invalidCredential
  case unavailable(OSStatus)
  case duplicate
  case unsafeTemporaryStorage

  var errorDescription: String? {
    switch self {
    case .invalidSnapshot: "The selected directory is not a supported recoverable snapshot."
    case .invalidCredential: "The local snapshot credential is malformed or outside safe limits."
    case .unavailable: "macOS Keychain could not complete the local snapshot unlock operation."
    case .duplicate: "A Keychain unlock already exists for this snapshot."
    case .unsafeTemporaryStorage:
      "GreenBubbles could not create owner-only temporary Keychain material."
    }
  }
}

struct SnapshotKeychainStore: Sendable {
  private static let service = "com.greenbubbles.snapshot-local-credential.v1"
  private static let maximumCredentialBytes = 1_024
  private static let maximumManifestBytes = 8 * 1_024 * 1_024

  func save(_ credential: Data, snapshotID: String) throws {
    try validateSnapshotID(snapshotID)
    try validateCredential(credential)
    let query: [CFString: Any] = [
      kSecClass: kSecClassGenericPassword,
      kSecAttrService: Self.service,
      kSecAttrAccount: snapshotID,
      kSecAttrAccessible: kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
      kSecAttrSynchronizable: false,
      kSecValueData: credential,
      kSecAttrLabel: "GreenBubbles snapshot local unlock",
    ]
    let status = SecItemAdd(query as CFDictionary, nil)
    if status == errSecDuplicateItem { throw SnapshotKeychainError.duplicate }
    guard status == errSecSuccess else { throw SnapshotKeychainError.unavailable(status) }
  }

  func load(snapshotID: String) throws -> Data {
    try validateSnapshotID(snapshotID)
    let query: [CFString: Any] = [
      kSecClass: kSecClassGenericPassword,
      kSecAttrService: Self.service,
      kSecAttrAccount: snapshotID,
      kSecAttrSynchronizable: false,
      kSecReturnData: true,
      kSecMatchLimit: kSecMatchLimitOne,
    ]
    var result: CFTypeRef?
    let status = SecItemCopyMatching(query as CFDictionary, &result)
    guard status == errSecSuccess, let data = result as? Data else {
      throw SnapshotKeychainError.unavailable(status)
    }
    try validateCredential(data)
    return data
  }

  func remove(snapshotID: String) throws {
    try validateSnapshotID(snapshotID)
    let query: [CFString: Any] = [
      kSecClass: kSecClassGenericPassword,
      kSecAttrService: Self.service,
      kSecAttrAccount: snapshotID,
      kSecAttrSynchronizable: false,
    ]
    let status = SecItemDelete(query as CFDictionary)
    guard status == errSecSuccess || status == errSecItemNotFound else {
      throw SnapshotKeychainError.unavailable(status)
    }
  }

  func snapshotID(at snapshotURL: URL) throws -> String {
    let manifest = snapshotURL.appending(path: "manifest.json")
    var metadata = stat()
    guard lstat(manifest.path, &metadata) == 0, metadata.st_mode & S_IFMT == S_IFREG,
      metadata.st_uid == getuid(), metadata.st_size > 0,
      metadata.st_size <= Self.maximumManifestBytes
    else { throw SnapshotKeychainError.invalidSnapshot }
    let data = try Data(contentsOf: manifest, options: [.mappedIfSafe])
    guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
      object["schema"] as? String == "greenbubbles.recoverable-snapshot.v2",
      object["formatVersion"] as? Int == 2,
      let snapshotID = object["snapshotId"] as? String
    else { throw SnapshotKeychainError.invalidSnapshot }
    try validateSnapshotID(snapshotID)
    return snapshotID
  }

  func materialize(
    snapshotURL: URL,
    sessionDirectory: URL
  ) throws -> URL {
    let snapshotID = try snapshotID(at: snapshotURL)
    let data = try load(snapshotID: snapshotID)
    try createPrivateDirectoryIfNeeded(sessionDirectory)
    let output = sessionDirectory.appending(
      path: ".snapshot-local-unlock-\(UUID().uuidString)"
    )
    let descriptor = open(
      output.path,
      O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW,
      S_IRUSR | S_IWUSR
    )
    guard descriptor >= 0 else { throw SnapshotKeychainError.unsafeTemporaryStorage }
    var created = true
    defer {
      close(descriptor)
      if created { unlink(output.path) }
    }
    try data.withUnsafeBytes { bytes in
      guard let base = bytes.baseAddress else { return }
      var offset = 0
      while offset < bytes.count {
        let count = Darwin.write(descriptor, base.advanced(by: offset), bytes.count - offset)
        guard count > 0 else { throw SnapshotKeychainError.unsafeTemporaryStorage }
        offset += count
      }
    }
    guard fsync(descriptor) == 0, fchmod(descriptor, S_IRUSR | S_IWUSR) == 0 else {
      throw SnapshotKeychainError.unsafeTemporaryStorage
    }
    created = false
    return output
  }

  private func validateSnapshotID(_ value: String) throws {
    guard value.count == 64,
      value.utf8.allSatisfy({ byte in
        (byte >= 48 && byte <= 57) || (byte >= 65 && byte <= 70)
          || (byte >= 97 && byte <= 102)
      })
    else { throw SnapshotKeychainError.invalidSnapshot }
  }

  private func validateCredential(_ data: Data) throws {
    guard !data.isEmpty, data.count <= Self.maximumCredentialBytes,
      let text = String(data: data, encoding: .utf8),
      text.hasPrefix("GREENBUBBLES LOCAL UNLOCK CREDENTIAL\nformat: 1\n"),
      text.contains("\ncredential-id: "), text.contains("\nsecret: ")
    else { throw SnapshotKeychainError.invalidCredential }
  }

  private func createPrivateDirectoryIfNeeded(_ url: URL) throws {
    if mkdir(url.path, S_IRWXU) != 0, errno != EEXIST {
      throw SnapshotKeychainError.unsafeTemporaryStorage
    }
    var metadata = stat()
    guard lstat(url.path, &metadata) == 0, metadata.st_mode & S_IFMT == S_IFDIR,
      metadata.st_uid == getuid(), metadata.st_mode & 0o077 == 0,
      chmod(url.path, S_IRWXU) == 0
    else { throw SnapshotKeychainError.unsafeTemporaryStorage }
  }
}
