import ImageIO
import SwiftUI
import UIKit
import XeniaCore

/// What's shared and a message to go with it, then the conversations, newest first: choose one
/// and Send. A suggestion picked from the share sheet's top row arrives already chosen.
struct ShareView: View {
    @Bindable var model: ShareModel
    @State private var confirmsStop = false

    var body: some View {
        NavigationStack {
            content
                .navigationTitle(ShareCopy.title)
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button(ShareCopy.cancel, role: .cancel) {
                            if model.isSending { confirmsStop = true } else { model.cancel() }
                        }
                    }
                    ToolbarItem(placement: .confirmationAction) {
                        Button(ShareCopy.send) { model.send() }
                            .disabled(!model.canSend)
                    }
                }
        }
        .interactiveDismissDisabled(model.isSending)
        .confirmationDialog(ShareCopy.stopTitle, isPresented: $confirmsStop, titleVisibility: .visible) {
            Button(ShareCopy.stop, role: .destructive) { model.cancel() }
            Button(ShareCopy.keepSending, role: .cancel) {}
        } message: {
            Text(ShareCopy.stopMessage)
        }
        .alert(ShareCopy.failedTitle, isPresented: failed) {
            Button(ShareCopy.tryAgain) { model.send() }
            Button(ShareCopy.sendLater) { model.sendLater() }
            Button(ShareCopy.cancel, role: .cancel) {}
        } message: {
            Text(model.failure ?? "")
        }
        .sensoryFeedback(.success, trigger: model.sentCount)
        .sensoryFeedback(trigger: model.failure) { _, new in new == nil ? nil : .error }
    }

    private var failed: Binding<Bool> {
        Binding(get: { model.failure != nil }, set: { if !$0 { model.failure = nil } })
    }

    @ViewBuilder
    private var content: some View {
        switch model.phase {
        case .opening:
            ProgressView()
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        case .locked:
            ContentUnavailableView {
                Label(ShareCopy.lockedTitle, systemImage: "lock.fill")
            } description: {
                Text(ShareCopy.lockedMessage)
            } actions: {
                Button(ShareCopy.unlock) { model.unlock() }
                    .buttonStyle(.borderedProminent)
            }
        case .signedOut:
            ContentUnavailableView(ShareCopy.signedOutTitle, systemImage: "person.crop.circle", description: Text(ShareCopy.signedOutMessage))
        case .failed:
            ContentUnavailableView(ShareCopy.openFailedTitle, systemImage: "exclamationmark.triangle", description: Text(ShareCopy.openFailedMessage))
        case .ready:
            list
        }
    }

    private var list: some View {
        List {
            Section {
                SharedPreview(content: model.content)
                    .listRowInsets(EdgeInsets(top: Theme.Spacing.m, leading: 0, bottom: Theme.Spacing.m, trailing: 0))
                TextField(ShareCopy.messagePlaceholder, text: $model.comment, axis: .vertical)
                    .lineLimit(1...4)
                    .disabled(model.isCommitted)
            } footer: {
                if let note {
                    Text(note)
                }
            }
            Section(ShareCopy.recent) {
                ForEach(model.shown, id: \.id) { target in
                    TargetRow(target: target, model: model, isSelected: target.id == model.selectedID) {
                        model.selectedID = target.id
                    }
                    .disabled(model.isCommitted)
                }
            }
        }
        .listStyle(.insetGrouped)
        .searchable(text: $model.query, placement: .navigationBarDrawer(displayMode: .always), prompt: ShareCopy.search)
        .overlay {
            if model.targets.isEmpty {
                ContentUnavailableView(ShareCopy.noConversationsTitle, systemImage: "bubble.left.and.bubble.right", description: Text(ShareCopy.noConversationsMessage))
            } else if model.shown.isEmpty {
                ContentUnavailableView.search(text: model.query)
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            if model.isSending, let target = model.selected {
                VStack(alignment: .leading, spacing: Theme.Spacing.s) {
                    Text(ShareCopy.sending(to: target.name))
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                    ProgressView(value: model.progress)
                }
                .padding(.horizontal, Theme.Spacing.l)
                .padding(.vertical, Theme.Spacing.m)
                .background(.bar)
                .transition(.move(edge: .bottom).combined(with: .opacity))
                .accessibilityElement(children: .combine)
            }
        }
        .animation(Theme.Motion.panel, value: model.isSending)
    }

    private var note: String? {
        guard let content = model.content else { return nil }
        if content.pieces.isEmpty { return ShareCopy.unreadable }
        if model.sendsLater { return ShareCopy.sendsLater }
        if content.hadFailures { return ShareCopy.someUnreadable }
        return nil
    }
}

private struct TargetRow: View {
    let target: ShareTarget
    let model: ShareModel
    let isSelected: Bool
    let action: () -> Void

    @Environment(\.displayScale) private var scale

    var body: some View {
        Button(action: action) {
            HStack(spacing: Theme.Spacing.m) {
                ShareAvatar(target: target, image: target.avatarUrl.flatMap { model.avatars[$0] }, size: Theme.Metrics.memberAvatar)
                Text(target.name)
                    .foregroundStyle(.primary)
                    .lineLimit(1)
                Spacer(minLength: 0)
                if isSelected {
                    Image(systemName: "checkmark")
                        .fontWeight(.semibold)
                        .foregroundStyle(.tint)
                        .transition(.scale(scale: 0.5).combined(with: .opacity))
                }
            }
            .contentShape(.rect)
            .animation(Theme.Motion.flip, value: isSelected)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text(ShareCopy.sendTo(target.name)))
        .accessibilityAddTraits(isSelected ? .isSelected : [])
        .task(id: target.avatarUrl) {
            await model.avatar(for: target, pixels: Int(Theme.Metrics.memberAvatar * scale))
        }
    }
}

/// A person's or room's photo, or their initials: people round, rooms rounded squares, as in
/// the app.
private struct ShareAvatar: View {
    let target: ShareTarget
    let image: UIImage?
    let size: CGFloat

    var body: some View {
        ZStack {
            Rectangle().fill(target.isDirect ? AnyShapeStyle(Color.monogram) : AnyShapeStyle(Color.person(target.avatarSeed).gradient))
            if let image {
                Image(uiImage: image)
                    .resizable()
                    .scaledToFill()
            } else {
                Text(initials(name: target.name))
                    .font(.system(size: size * (target.isDirect ? 0.4 : 0.38), weight: .semibold, design: .rounded))
                    .foregroundStyle(.white)
                    .minimumScaleFactor(0.5)
                    .lineLimit(1)
                    .padding(size * 0.1)
            }
        }
        .frame(width: size, height: size)
        .clipShape(target.isDirect ? AnyShape(Circle()) : AnyShape(RoundedRectangle(cornerRadius: size * 0.25, style: .continuous)))
        .accessibilityHidden(true)
    }
}

/// What's being shared, as a row of small tiles.
private struct SharedPreview: View {
    let content: SharedContent?

    private static let side: CGFloat = 72

    var body: some View {
        ScrollView(.horizontal) {
            HStack(spacing: Theme.Spacing.s) {
                if let content {
                    ForEach(content.pieces) { piece in
                        Tile(piece: piece, side: Self.side)
                    }
                } else {
                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                        .fill(.quaternary)
                        .frame(width: Self.side, height: Self.side)
                        .overlay { ProgressView() }
                }
            }
            .padding(.horizontal, Theme.Spacing.l)
        }
        .scrollIndicators(.hidden)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(Text(ShareCopy.items(content?.pieces.count ?? 0)))
    }
}

private struct Tile: View {
    let piece: SharedContent.Piece
    let side: CGFloat

    @Environment(\.displayScale) private var scale
    @State private var thumbnail: UIImage?

    var body: some View {
        switch piece {
        case .text(let text):
            card(symbol: "text.alignleft", text: text)
        case .link(let url):
            card(symbol: "link", text: url.host() ?? url.absoluteString)
        case .file(let attachment, let kind):
            ZStack {
                RoundedRectangle(cornerRadius: 10, style: .continuous).fill(.quaternary)
                if let thumbnail {
                    Image(uiImage: thumbnail)
                        .resizable()
                        .scaledToFill()
                } else if kind == .file {
                    VStack(spacing: Theme.Spacing.xs) {
                        Image(systemName: "doc.fill")
                            .font(.title2)
                            .foregroundStyle(.secondary)
                        Text(attachment.filename)
                            .font(.caption2)
                            .lineLimit(2)
                            .multilineTextAlignment(.center)
                    }
                    .padding(Theme.Spacing.xs)
                }
                if kind == .video {
                    Image(systemName: "play.fill")
                        .font(.title3)
                        .foregroundStyle(.white)
                        .shadow(radius: 2)
                }
            }
            .frame(width: side, height: side)
            .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
            .task(id: attachment.path) {
                guard kind != .file else { return }
                let source = kind == .video ? attachment.thumbnailPath : attachment.path
                guard let source else { return }
                thumbnail = await Self.thumbnail(of: source, pixels: side * scale)
            }
        }
    }

    private func card(symbol: String, text: String) -> some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
            Image(systemName: symbol)
                .foregroundStyle(.secondary)
            Text(text)
                .font(.caption2)
                .lineLimit(3)
        }
        .padding(Theme.Spacing.s)
        .frame(width: side * 2, height: side, alignment: .topLeading)
        .background(.quaternary, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
    }

    /// A small picture of a photo, read without decoding the whole of it.
    @concurrent
    private static func thumbnail(of path: String, pixels: CGFloat) async -> UIImage? {
        guard let source = CGImageSourceCreateWithURL(URL(filePath: path) as CFURL, nil) else { return nil }
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: pixels,
        ]
        return CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary).map { UIImage(cgImage: $0) }
    }
}
