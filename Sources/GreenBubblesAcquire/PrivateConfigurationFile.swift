import Darwin
import Foundation

/// Reads a small owner-only configuration file with the rules the Rust CLI
/// applies in `read_private_file` (`Native/GreenBubbles/src/query_profile.rs`):
/// an absolute path; a parent that is a real directory owned by the user with
/// no group or other access; a regular, single-link, owner-only file owned by
/// the user, between 1 byte and the size limit. The file is opened with
/// `O_NOFOLLOW` and checked again through the open descriptor, so a swap
/// between the check and the read is caught.
///
/// Call it inside `InvokingUser.perform(_:)` so that the open itself runs
/// with the user's rights.
public enum PrivateConfigurationFile {
  public static let maximumBytes = 64 * 1024

  public enum ReadResult: Equatable {
    /// The file or its parent does not exist or cannot be inspected; the
    /// Rust CLI treats this as "no file".
    case absent
    case contents(Data)
  }

  public static func read(
    _ url: URL,
    ownerUID: uid_t,
    description: String,
    maximumBytes: Int = maximumBytes
  ) throws -> ReadResult {
    let path = url.path
    guard path.hasPrefix("/") else {
      throw QueryProfileError.unsafeFile("\(description) path must be absolute")
    }
    let parentPath = url.deletingLastPathComponent().path
    var parent = stat()
    guard lstat(parentPath, &parent) == 0 else { return .absent }
    guard parent.st_mode & S_IFMT == S_IFDIR, parent.st_uid == ownerUID,
      parent.st_mode & 0o077 == 0
    else {
      throw QueryProfileError.unsafeFile(
        "the folder holding the \(description) must be a real folder owned by you "
          + "with no group or other access (chmod 700)")
    }
    var info = stat()
    guard lstat(path, &info) == 0 else { return .absent }
    guard info.st_mode & S_IFMT != S_IFLNK else {
      throw QueryProfileError.unsafeFile("the \(description) must not be a symbolic link")
    }
    try validate(info, ownerUID: ownerUID, description: description, maximumBytes: maximumBytes)

    let descriptor = open(path, O_RDONLY | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK)
    guard descriptor >= 0 else {
      if errno == ENOENT { return .absent }
      throw QueryProfileError.unsafeFile("the \(description) could not be opened (POSIX \(errno))")
    }
    defer { close(descriptor) }
    var opened = stat()
    guard fstat(descriptor, &opened) == 0 else {
      throw QueryProfileError.unsafeFile("the \(description) could not be inspected")
    }
    try validate(opened, ownerUID: ownerUID, description: description, maximumBytes: maximumBytes)

    var bytes = [UInt8](repeating: 0, count: maximumBytes + 1)
    var total = 0
    while total < bytes.count {
      let count = bytes.withUnsafeMutableBytes { buffer in
        Darwin.read(descriptor, buffer.baseAddress!.advanced(by: total), buffer.count - total)
      }
      if count < 0 {
        if errno == EINTR { continue }
        throw QueryProfileError.unsafeFile("the \(description) could not be read")
      }
      if count == 0 { break }
      total += count
    }
    guard total > 0, total <= maximumBytes else {
      throw QueryProfileError.unsafeFile("the \(description) size is outside safe limits")
    }
    return .contents(Data(bytes.prefix(total)))
  }

  private static func validate(
    _ info: stat,
    ownerUID: uid_t,
    description: String,
    maximumBytes: Int
  ) throws {
    guard info.st_mode & S_IFMT == S_IFREG, info.st_uid == ownerUID, info.st_nlink == 1,
      info.st_mode & 0o077 == 0
    else {
      throw QueryProfileError.unsafeFile(
        "the \(description) must be a regular file owned by you, with one hard link "
          + "and no group or other access (chmod 600)")
    }
    guard info.st_size > 0, info.st_size <= off_t(maximumBytes) else {
      throw QueryProfileError.unsafeFile("the \(description) size is outside safe limits")
    }
  }
}
