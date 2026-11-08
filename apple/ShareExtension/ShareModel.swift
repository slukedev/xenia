import Foundation
import Intents
import LocalAuthentication
import Observation
import OSLog
import UIKit
import XeniaCore

/// The share sheet's state: the session opened from the app's stores, the conversations to choose
/// from, what's being shared, and the send.
@Observable
final class ShareModel {
    enum Phase: Equatable {
        case opening, locked, signedOut, failed, ready
    }

    private(set) var phase = Phase.opening
    private(set) var targets: [ShareTarget] = []
    var query = ""
    var selectedID: String?
    var comment = ""
    /// Nil while the shared items are still being read.
    private(set) var content: SharedContent?
    private(set) var sendsLater = false
    private(set) var isSending = false
    private(set) var progress: Double = 0
    /// A send that failed, worded.
    var failure: String?
    private(set) var avatars: [String: UIImage] = [:]
    /// Bumped when the share went, for the haptic.
    private(set) var sentCount = 0

    /// Called once: true when everything was sent (or handed to the app), false when cancelled.
    @ObservationIgnored var onFinish: ((Bool) -> Void)?
    @ObservationIgnored private var session: ShareSession?
    @ObservationIgnored private var pending: (roomID: String, share: PendingShare)?
    @ObservationIgnored private var sendTask: Task<Void, Never>?
    @ObservationIgnored private var suggestedID: String?

    private static let log = Logger(subsystem: "dev.xenia.matrix", category: "share")

    var shown: [ShareTarget] {
        let trimmed = query.trimmingCharacters(in: .whitespaces)
        return trimmed.isEmpty ? targets : filterShareTargets(targets: targets, query: trimmed)
    }

    var selected: ShareTarget? {
        targets.first { $0.id == selectedID }
    }

    var canSend: Bool {
        phase == .ready && selected != nil && !isSending && !(content?.pieces.isEmpty ?? true)
    }

    /// Once something has gone, the share can't change: trying again sends the rest of it.
    var isCommitted: Bool { isSending || pending != nil }

    /// Opens the session and reads what's shared, side by side.
    func start(items: [NSExtensionItem], intent: INIntent?) {
        suggestedID = (intent as? INSendMessageIntent)?.conversationIdentifier
        selectedID = suggestedID
        Task {
            let content = await SharedContent.load(items)
            self.content = content
            sendsLater = !content.pieces.isEmpty && !sharesFromExtension(items: content.items)
        }
        if AppGroup.isAppLockOn {
            phase = .locked
            unlock()
        } else {
            Task { await open() }
        }
    }

    func unlock() {
        Task {
            let context = LAContext()
            do {
                guard try await context.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: ShareCopy.unlockReason) else { return }
                phase = .opening
                await open()
            } catch {
                // Cancelled, or no passcode: the lock's message stays, with its button.
            }
        }
    }

    private func open() async {
        do {
            guard let session = try await openShareSession(
                config: AppEnvironment.clientConfig(forExtension: true),
                secrets: AppEnvironment.secretStore()
            ) else {
                phase = .signedOut
                return
            }
            self.session = session
            var targets = await session.conversations()
            // A suggestion the system offered goes first, already chosen.
            if let suggestedID, let index = targets.firstIndex(where: { $0.id == suggestedID }) {
                targets.insert(targets.remove(at: index), at: 0)
            } else {
                selectedID = nil
            }
            self.targets = targets
            phase = .ready
        } catch {
            Self.log.error("Couldn't open the session: \(String(describing: error), privacy: .private)")
            phase = .failed
        }
    }

    func avatar(for target: ShareTarget, pixels: Int) async {
        guard let url = target.avatarUrl, avatars[url] == nil, let session else { return }
        guard let data = try? await session.loadAvatar(mxcUrl: url, sizePx: UInt32(pixels)),
              let image = await Self.decode(data) else { return }
        avatars[url] = image
    }

    @concurrent
    private static func decode(_ data: Data) async -> UIImage? {
        await UIImage(data: data)?.byPreparingForDisplay()
    }

    func send() {
        guard canSend, let session, let target = selected, let content else { return }
        failure = nil
        let share: PendingShare
        if let pending, pending.roomID == target.id {
            share = pending.share
        } else {
            do {
                share = try session.prepare(roomId: target.id, items: content.items, comment: comment)
            } catch {
                failure = Copy.error(error)
                return
            }
            pending = (target.id, share)
        }
        isSending = true
        sendTask = Task {
            let watcher = Task {
                while !Task.isCancelled {
                    progress = share.progress()
                    try? await Task.sleep(for: .milliseconds(100))
                }
            }
            defer { watcher.cancel() }
            do {
                let outcome = try await share.send()
                progress = 1
                if outcome == .sent { Self.donate(target) }
                finish(sent: true)
            } catch {
                if Task.isCancelled { return }
                Self.log.error("Couldn't share: \(String(describing: error), privacy: .private)")
                isSending = false
                // Nothing went yet: the choice and the message can still change.
                if share.nothingSent() { pending = nil }
                failure = Copy.error(error)
            }
        }
    }

    /// After a failure: what's left goes to the app, which sends it when it next opens.
    func sendLater() {
        guard let share = pending?.share else { return }
        Task {
            do {
                try await share.handOff()
                finish(sent: true)
            } catch {
                failure = Copy.error(error)
            }
        }
    }

    func cancel() {
        sendTask?.cancel()
        finish(sent: false)
    }

    private func finish(sent: Bool) {
        if sent { sentCount += 1 }
        let content = content
        let finish = onFinish
        onFinish = nil
        Task {
            // Long enough to feel the send land.
            if sent { try? await Task.sleep(for: .milliseconds(250)) }
            finish?(sent)
            if let content { SharedContent.remove(content) }
        }
    }

    /// Tells the system who was shared to, so the share sheet suggests the conversation next
    /// time. Names and IDs only, as the app's own donations.
    private static func donate(_ target: ShareTarget) {
        let recipients = target.dmUserId.map { id in
            [INPerson(
                personHandle: INPersonHandle(value: id, type: .unknown),
                nameComponents: nil,
                displayName: target.name,
                image: nil,
                contactIdentifier: nil,
                customIdentifier: id
            )]
        }
        let intent = INSendMessageIntent(
            recipients: recipients,
            outgoingMessageType: .outgoingMessageText,
            content: nil,
            speakableGroupName: target.dmUserId == nil ? INSpeakableString(spokenPhrase: target.name) : nil,
            conversationIdentifier: target.id,
            serviceName: nil,
            sender: nil,
            attachments: nil
        )
        let interaction = INInteraction(intent: intent, response: nil)
        interaction.direction = .outgoing
        interaction.groupIdentifier = target.id
        Task {
            do {
                try await interaction.donate()
            } catch {
                log.error("Couldn't donate a share: \(String(describing: error), privacy: .private)")
            }
        }
    }
}
