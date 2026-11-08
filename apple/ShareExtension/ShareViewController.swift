import Intents
import SwiftUI
import UIKit

/// Xenia in the share sheet: what's shared goes to a conversation chosen from the list, with an
/// optional message, as in Messages. The extension opens the app's session from the shared
/// stores; it never syncs.
final class ShareViewController: UIViewController {
    private let model = ShareModel()

    override func viewDidLoad() {
        super.viewDidLoad()
        let host = UIHostingController(rootView: ShareView(model: model))
        addChild(host)
        host.view.frame = view.bounds
        host.view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        view.addSubview(host.view)
        host.didMove(toParent: self)

        model.onFinish = { [weak self] sent in
            guard let context = self?.extensionContext else { return }
            if sent {
                context.completeRequest(returningItems: nil)
            } else {
                context.cancelRequest(withError: CocoaError(.userCancelled))
            }
        }
        let items = extensionContext?.inputItems.compactMap { $0 as? NSExtensionItem } ?? []
        model.start(items: items, intent: extensionContext?.intent)
    }
}
