import XeniaCore
import SwiftUI

/// The row over a list that leads to its archived conversations, as WhatsApp's does. Their unread
/// count is plain text, not a badge: what's archived asked not to call for attention.
struct ArchivedRow: View {
    let summary: ArchiveSummary

    var body: some View {
        HStack(spacing: Theme.Spacing.m) {
            Image(systemName: "archivebox")
                .font(.title3)
                .foregroundStyle(.secondary)
                .frame(width: Theme.Metrics.rowAvatar)
            Text("Archived")
                .font(.body.weight(.semibold))
            Spacer(minLength: Theme.Spacing.s)
            if summary.unread > 0 {
                Text(Int(summary.unread), format: .number)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, Theme.Spacing.xs)
        .contentShape(.rect)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text("Archived"))
        .accessibilityValue(Text(Copy.archived(summary)))
        .accessibilityAddTraits(.isButton)
    }
}
