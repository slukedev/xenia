import Foundation
import ImageIO
import OSLog
import UniformTypeIdentifiers
import XeniaCore

/// What another app shared, read into things the core can send: text and links as they are,
/// photos, videos and files copied out of the providers (whose files only last while they're
/// read) and prepared the way the composer prepares a picked photo.
struct SharedContent {
    enum Piece: Identifiable {
        case text(String)
        case link(URL)
        case file(Attachment, kind: Kind)

        enum Kind { case photo, video, file }

        var id: String {
            switch self {
            case .text(let text): "text:\(text)"
            case .link(let url): "link:\(url.absoluteString)"
            case .file(let attachment, _): "file:\(attachment.path)"
            }
        }

        var item: SharedItem {
            switch self {
            case .text(let text): .text(text: text)
            case .link(let url): .text(text: url.absoluteString)
            case .file(let attachment, _): .file(attachment: attachment)
            }
        }
    }

    var pieces: [Piece] = []
    /// Some providers couldn't be read; the rest still go.
    var hadFailures = false

    var items: [SharedItem] { pieces.map(\.item) }

    private static let log = Logger(subsystem: "dev.xenia.matrix", category: "share")

    /// Reads every attachment of every shared item, in order.
    static func load(_ inputItems: [NSExtensionItem]) async -> SharedContent {
        var content = SharedContent()
        for provider in inputItems.flatMap({ $0.attachments ?? [] }) {
            do {
                if let piece = try await piece(from: provider) {
                    content.pieces.append(piece)
                }
            } catch {
                log.error("Couldn't read a shared item: \(String(describing: error), privacy: .private)")
                content.hadFailures = true
            }
        }
        return content
    }

    /// Done sending, or cancelled: the copies go.
    static func remove(_ content: SharedContent) {
        for piece in content.pieces {
            if case .file(let attachment, _) = piece { AttachmentMaker.remove(attachment) }
        }
        try? FileManager.default.removeItem(at: PickedMedia.folder)
    }

    private static func piece(from provider: NSItemProvider) async throws -> Piece? {
        if provider.hasItemConformingToTypeIdentifier(UTType.movie.identifier) {
            let file = try await copy(from: provider, as: .movie)
            let attachment = await AttachmentMaker.withVideoDetails(try await AttachmentMaker.fromFile(file))
            return .file(attachment, kind: .video)
        }
        if provider.hasItemConformingToTypeIdentifier(UTType.image.identifier) {
            let file: URL
            do {
                file = try await copy(from: provider, as: .image)
            } catch {
                // A screenshot from the markup editor comes as image data, not a file.
                let (data, type) = try await data(from: provider, as: .image)
                file = try await PickedMedia.saving(data, as: type).url
            }
            return .file(try await AttachmentMaker.fromFile(file), kind: .photo)
        }
        if provider.hasItemConformingToTypeIdentifier(UTType.url.identifier) {
            let url = try await loadURL(from: provider)
            if url.isFileURL {
                return .file(try await AttachmentMaker.fromFile(try keep(url)), kind: .file)
            }
            return .link(url)
        }
        if provider.hasItemConformingToTypeIdentifier(UTType.plainText.identifier) {
            let text = try await loadText(from: provider).trimmingCharacters(in: .whitespacesAndNewlines)
            return text.isEmpty ? nil : .text(text)
        }
        if provider.hasItemConformingToTypeIdentifier(UTType.data.identifier) {
            return .file(try await AttachmentMaker.fromFile(try await copy(from: provider, as: .data)), kind: .file)
        }
        return nil
    }

    /// The provider's file, copied while it's readable.
    private static func copy(from provider: NSItemProvider, as type: UTType) async throws -> URL {
        try await withCheckedThrowingContinuation { continuation in
            _ = provider.loadFileRepresentation(for: type, openInPlace: false) { url, _, error in
                guard let url else {
                    continuation.resume(throwing: error ?? CocoaError(.fileReadUnknown))
                    return
                }
                continuation.resume(with: Result { try keep(url) })
            }
        }
    }

    private static func data(from provider: NSItemProvider, as type: UTType) async throws -> (Data, UTType) {
        let identifier = provider.registeredContentTypes.first { $0.conforms(to: type) } ?? .png
        let data: Data = try await withCheckedThrowingContinuation { continuation in
            _ = provider.loadDataRepresentation(for: identifier) { data, error in
                guard let data else {
                    continuation.resume(throwing: error ?? CocoaError(.fileReadUnknown))
                    return
                }
                continuation.resume(returning: data)
            }
        }
        return (data, identifier)
    }

    private static func loadURL(from provider: NSItemProvider) async throws -> URL {
        try await withCheckedThrowingContinuation { continuation in
            _ = provider.loadObject(ofClass: URL.self) { url, error in
                guard let url else {
                    continuation.resume(throwing: error ?? CocoaError(.fileReadUnknown))
                    return
                }
                continuation.resume(returning: url)
            }
        }
    }

    private static func loadText(from provider: NSItemProvider) async throws -> String {
        try await withCheckedThrowingContinuation { continuation in
            _ = provider.loadObject(ofClass: String.self) { text, error in
                guard let text else {
                    continuation.resume(throwing: error ?? CocoaError(.fileReadUnknown))
                    return
                }
                continuation.resume(returning: text)
            }
        }
    }

    /// A copy in the extension's own temporary folder, under its own name.
    private nonisolated static func keep(_ file: URL) throws -> URL {
        let accessing = file.startAccessingSecurityScopedResource()
        defer { if accessing { file.stopAccessingSecurityScopedResource() } }
        let folder = PickedMedia.folder.appending(path: UUID().uuidString, directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let destination = folder.appending(path: file.lastPathComponent)
        try FileManager.default.copyItem(at: file, to: destination)
        return destination
    }
}
