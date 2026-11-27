import SwiftUI
import UIKit

/// Applies a measured row delta to the scroll view that produced the layout.
/// The current offset and insets are read together at the time of correction.
@MainActor
final class TranscriptScrollOffset {
    weak var scrollView: UIScrollView?

    enum Shift {
        case unchanged
        case shifted
        case needsTrailingSpace(CGFloat, target: CGFloat)
    }

    func followBottom() {
        guard let scrollView, !scrollView.isTracking,
              !scrollView.isDragging, !scrollView.isDecelerating else { return }
        let insets = scrollView.adjustedContentInset
        let current = scrollView.contentOffset
        let bottom = max(-insets.top, scrollView.contentSize.height + insets.bottom - scrollView.bounds.height)
        guard abs(current.y - bottom) > 0.5 else { return }
        scrollView.setContentOffset(CGPoint(x: current.x, y: bottom), animated: false)
    }

    func shift(by correction: CGFloat) -> Shift {
        guard let scrollView else { return .unchanged }
        let insets = scrollView.adjustedContentInset
        let current = scrollView.contentOffset
        let offset = current.y + insets.top
        let extent = scrollView.contentSize.height + insets.top + insets.bottom - scrollView.bounds.height
        let space = TranscriptRowPosition.trailingSpace(from: offset, maximum: extent, correction: correction)
        if space > 0.5 {
            return .needsTrailingSpace(space, target: offset + correction)
        }
        guard let y = TranscriptRowPosition.restorationOffset(
            from: offset, maximum: extent, correction: correction
        ) else { return .unchanged }
        scrollView.setContentOffset(CGPoint(x: current.x, y: y - insets.top), animated: false)
        return .shifted
    }

    func completeTrailingShift(to target: CGFloat) -> Bool {
        guard let scrollView else { return false }
        let insets = scrollView.adjustedContentInset
        let current = scrollView.contentOffset
        let extent = scrollView.contentSize.height + insets.top + insets.bottom - scrollView.bounds.height
        guard target <= extent + 0.5 else { return false }
        let y = min(extent, target) - insets.top
        if abs(y - current.y) > 0.5 {
            scrollView.setContentOffset(CGPoint(x: current.x, y: y), animated: false)
        }
        return true
    }
}

struct TranscriptScrollOffsetProbe: UIViewRepresentable {
    let offset: TranscriptScrollOffset

    func makeUIView(context: Context) -> ProbeView {
        let view = ProbeView()
        view.isUserInteractionEnabled = false
        view.accessibilityElementsHidden = true
        view.offset = offset
        return view
    }

    func updateUIView(_ uiView: ProbeView, context: Context) {
        uiView.offset = offset
        uiView.findScrollView()
    }

    final class ProbeView: UIView {
        weak var offset: TranscriptScrollOffset?

        override func didMoveToSuperview() {
            super.didMoveToSuperview()
            findScrollView()
        }

        override func didMoveToWindow() {
            super.didMoveToWindow()
            findScrollView()
        }

        func findScrollView() {
            var ancestor = superview
            while let view = ancestor {
                if let scrollView = view as? UIScrollView {
                    offset?.scrollView = scrollView
                    return
                }
                ancestor = view.superview
            }
        }
    }
}
