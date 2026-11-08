import AVFoundation
import SwiftUI
#if os(macOS)
import AppKit
#else
import UIKit
#endif

/// A video playing muted, on a loop and without controls, as a GIF does. It runs only while
/// `isPlaying`, and holds its frame otherwise.
struct LoopingVideo: View {
    let url: URL
    let isPlaying: Bool
    /// Filling its frame, as a bubble crops; the viewer fits it instead.
    var fills = true

    var body: some View {
        LoopingVideoRepresentable(url: url, isPlaying: isPlaying, gravity: fills ? .resizeAspectFill : .resizeAspect)
            .allowsHitTesting(false)
            .accessibilityHidden(true)
    }
}

final class LoopingVideoView: PlatformView {
    private var looper: AVPlayerLooper?
    private var url: URL?

    #if os(macOS)
    override func makeBackingLayer() -> CALayer { AVPlayerLayer() }
    override func hitTest(_ point: NSPoint) -> NSView? { nil }
    #else
    override class var layerClass: AnyClass { AVPlayerLayer.self }
    override func hitTest(_ point: CGPoint, with event: UIEvent?) -> UIView? { nil }
    #endif

    private var playerLayer: AVPlayerLayer? { layer as? AVPlayerLayer }

    func show(_ url: URL, playing: Bool, gravity: AVLayerVideoGravity) {
        guard let playerLayer else { return }
        playerLayer.videoGravity = gravity
        if url != self.url {
            stop()
            self.url = url
            let player = AVQueuePlayer()
            player.isMuted = true
            // A GIF mustn't keep the screen awake.
            player.preventsDisplaySleepDuringVideoPlayback = false
            looper = AVPlayerLooper(player: player, templateItem: AVPlayerItem(url: url))
            playerLayer.player = player
        }
        if playing {
            playerLayer.player?.play()
        } else {
            playerLayer.player?.pause()
        }
    }

    func stop() {
        playerLayer?.player?.pause()
        looper?.disableLooping()
        looper = nil
        playerLayer?.player = nil
        url = nil
    }
}

#if os(macOS)
private struct LoopingVideoRepresentable: NSViewRepresentable {
    let url: URL
    let isPlaying: Bool
    let gravity: AVLayerVideoGravity

    func makeNSView(context: Context) -> LoopingVideoView {
        let view = LoopingVideoView()
        view.wantsLayer = true
        return view
    }

    func updateNSView(_ view: LoopingVideoView, context: Context) {
        view.show(url, playing: isPlaying, gravity: gravity)
    }

    static func dismantleNSView(_ view: LoopingVideoView, coordinator: ()) {
        view.stop()
    }
}
#else
private struct LoopingVideoRepresentable: UIViewRepresentable {
    let url: URL
    let isPlaying: Bool
    let gravity: AVLayerVideoGravity

    func makeUIView(context: Context) -> LoopingVideoView { LoopingVideoView() }

    func updateUIView(_ view: LoopingVideoView, context: Context) {
        view.show(url, playing: isPlaying, gravity: gravity)
    }

    static func dismantleUIView(_ view: LoopingVideoView, coordinator: ()) {
        view.stop()
    }
}
#endif
