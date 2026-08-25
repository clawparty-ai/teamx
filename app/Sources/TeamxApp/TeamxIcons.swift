//
//  TeamxIcons.swift — programmatic status-bar icon (letter badge).
//
//  Draws a compact "Tx" monogram with the system font so it stays crisp at
//  any menu-bar scale and adapts to light/dark mode via `isTemplate`.
//

import AppKit

enum TeamxIcons {
/// Status-bar icon: white rounded-square background with a blue "T"
/// (matches the teamx logo palette). Fixed colors, not a template.
static func statusBarImage(size: CGFloat = 18) -> NSImage? {
    let canvas = NSSize(width: size, height: size)
    let image = NSImage(size: canvas)
    image.lockFocus()

    // White rounded-square background.
    let bgRect = NSRect(x: 0, y: 0, width: size, height: size)
    let bgPath = NSBezierPath(roundedRect: bgRect, xRadius: size * 0.22, yRadius: size * 0.22)
    NSColor.white.setFill()
    bgPath.fill()

    // Blue "T".
    let font = NSFont.boldSystemFont(ofSize: size * 0.70)
    let attrs: [NSAttributedString.Key: Any] = [
        .font: font,
        .foregroundColor: NSColor(srgbRed: 0.20, green: 0.45, blue: 0.95, alpha: 1.0),
    ]
    let text = NSAttributedString(string: "T", attributes: attrs)
    let textSize = text.size()
    let origin = NSPoint(
        x: (canvas.width - textSize.width) / 2,
        y: (canvas.height - textSize.height) / 2
    )
    text.draw(at: origin)

    image.unlockFocus()
    // Not template: we intentionally use fixed white/blue.
    return image
}
}
