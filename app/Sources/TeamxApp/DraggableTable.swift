//
//  DraggableTable.swift — a table whose height can be adjusted by dragging a
//  bottom handle. Height is persisted per-identifier in UserDefaults.
//

import AppKit

final class DraggableTable: NSView {
    private let table: SimpleTable
    private let handle = NSView()
    private let id: String
    private var startHeight: CGFloat = 0
    private var startY: CGFloat = 0

    init(columns: [String], widths: [CGFloat] = [], id: String, defaultHeight: CGFloat = 90) {
        self.id = id
        table = SimpleTable(columns: columns, widths: widths)
        super.init(frame: .zero)
        self.translatesAutoresizingMaskIntoConstraints = false

        addSubview(table)
        addSubview(handle)

        // Table fills above the handle.
        table.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([
            table.topAnchor.constraint(equalTo: topAnchor),
            table.leadingAnchor.constraint(equalTo: leadingAnchor),
            table.trailingAnchor.constraint(equalTo: trailingAnchor),
            table.bottomAnchor.constraint(equalTo: handle.topAnchor),
        ])

        // Drag handle: 6pt tall, subtle background.
        handle.wantsLayer = true
        handle.layer?.backgroundColor = NSColor.separatorColor.cgColor
        handle.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([
            handle.leadingAnchor.constraint(equalTo: leadingAnchor),
            handle.trailingAnchor.constraint(equalTo: trailingAnchor),
            handle.bottomAnchor.constraint(equalTo: bottomAnchor),
            handle.heightAnchor.constraint(equalToConstant: 6),
        ])

        // Height: persisted, clamped to a sensible range.
        let saved = UserDefaults.standard.double(forKey: "table.height.\(id)")
        let h = saved > 40 ? CGFloat(saved) : defaultHeight
        heightAnchor.constraint(equalToConstant: h).isActive = true
    }

    required init?(coder: NSCoder) { fatalError() }

    func setRows(_ rows: [[String]]) { table.setRows(rows) }

    private var dragging = false

    override func mouseDown(with event: NSEvent) {
        let p = convert(event.locationInWindow, from: nil)
        guard handle.frame.contains(p) else {
            super.mouseDown(with: event)
            return
        }
        dragging = true
        startHeight = frame.height
        startY = event.locationInWindow.y
    }

    override func mouseDragged(with event: NSEvent) {
        guard dragging else {
            super.mouseDragged(with: event)
            return
        }
        let dy = event.locationInWindow.y - startY
        let newHeight = min(max(startHeight - dy, 60), 500)
        for c in constraints {
            if c.firstAttribute == .height {
                c.constant = newHeight
            }
        }
    }

    override func mouseUp(with event: NSEvent) {
        if dragging {
            // Persist once at drag end, not on every frame.
            UserDefaults.standard.set(Double(frame.height), forKey: "table.height.\(id)")
            dragging = false
        }
        super.mouseUp(with: event)
    }
}
