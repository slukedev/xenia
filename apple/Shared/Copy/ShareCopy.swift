import Foundation

/// The words of the share sheet extension: sending what's shared from another app to a
/// conversation.
enum ShareCopy {
    static let title = String(localized: "Send To", comment: "The share sheet's title: choose a conversation")
    static let send = String(localized: "Send")
    static let cancel = String(localized: "Cancel")
    static let search = String(localized: "Search Conversations")
    static let messagePlaceholder = String(localized: "Add a message", comment: "The optional message sent with what's shared")
    static let recent = String(localized: "Conversations")

    static let signedOutTitle = String(localized: "Sign In to Xenia")
    static let signedOutMessage = String(localized: "Open Xenia and sign in, then share again.")
    static let noConversationsTitle = String(localized: "No Conversations")
    static let noConversationsMessage = String(localized: "Conversations you join in Xenia appear here.")
    static let lockedTitle = String(localized: "Xenia Is Locked")
    static let lockedMessage = String(localized: "Unlock Xenia to choose a conversation.")
    static let unlock = String(localized: "Unlock")
    static let unlockReason = String(localized: "Unlock Xenia to share to a conversation.")
    static let openFailedTitle = String(localized: "Can’t Share Right Now")
    static let openFailedMessage = String(localized: "Xenia couldn’t open your conversations. Try again in a moment.")
    static let unreadable = String(localized: "Xenia couldn’t read what you’re sharing.")
    static let someUnreadable = String(localized: "Xenia couldn’t read some of what you’re sharing. The rest will be sent.")
    static let sendsLater = String(localized: "This is too large to send from here. Xenia will send it the next time you open it.")

    static let failedTitle = String(localized: "Couldn’t Send")
    static let tryAgain = String(localized: "Try Again")
    static let sendLater = String(localized: "Send When Xenia Opens")
    static let stopTitle = String(localized: "Stop Sending?")
    static let stopMessage = String(localized: "Anything that hasn’t been sent yet won’t be sent.")
    static let stop = String(localized: "Stop Sending")
    static let keepSending = String(localized: "Keep Sending")

    /// While it uploads: "Sending to Maya…"
    static func sending(to name: String) -> String {
        String(localized: "Sending to \(name)…")
    }

    static func sendTo(_ name: String) -> String {
        String(localized: "Send to \(name)", comment: "VoiceOver: choose this conversation")
    }

    /// What's being shared, for VoiceOver and the preview's caption: "3 items".
    static func items(_ count: Int) -> String {
        String(inflecting: "^[\(count) item](inflect: true)")
    }
}
