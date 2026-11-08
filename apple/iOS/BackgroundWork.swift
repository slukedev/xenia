import UIKit

/// Time from iOS to finish work after leaving the screen. Suspended halfway through a write to
/// the shared stores, with a file locked, the app is killed (0xdead10cc).
final class BackgroundWork {
    private var id: UIBackgroundTaskIdentifier = .invalid

    init(_ name: String) {
        id = UIApplication.shared.beginBackgroundTask(withName: name) { [self] in
            // Out of time: iOS kills an app that keeps the task past this.
            end()
        }
    }

    /// Safe to call more than once: the work and the time limit can both end it.
    func end() {
        guard id != .invalid else { return }
        UIApplication.shared.endBackgroundTask(id)
        id = .invalid
    }
}
