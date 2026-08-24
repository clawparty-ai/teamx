//
//  SimpleTable.swift — a lightweight NSTableView wrapper for the panel.
//
//  Data is a list of rows, each row is a list of cell strings. Column headers
//  are fixed. Rows auto-size; the scroll view fills its container.
//

import AppKit

final class SimpleTable: NSView {
    private let tableView = NSTableView()
    private let scroll = NSScrollView()
    private var rows: [[String]] = []
    private var columnCount = 0

    /// `columns`: header titles. `widths`: optional fixed widths (0 = auto).
    init(columns: [String], widths: [CGFloat] = []) {
        super.init(frame: .zero)
        columnCount = columns.count

        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = false
        scroll.translatesAutoresizingMaskIntoConstraints = false
        scroll.documentView = tableView
        addSubview(scroll)
        NSLayoutConstraint.activate([
            scroll.topAnchor.constraint(equalTo: topAnchor),
            scroll.leadingAnchor.constraint(equalTo: leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: trailingAnchor),
            scroll.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])

        tableView.usesAlternatingRowBackgroundColors = true
        tableView.rowHeight = 20
        tableView.allowsColumnReordering = false
        tableView.dataSource = self
        tableView.delegate = self

        for (i, title) in columns.enumerated() {
            let col = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("col\(i)"))
            col.title = title
            col.headerCell.alignment = .left
            if i < widths.count, widths[i] > 0 {
                col.width = widths[i]
            } else {
                col.width = 100
                col.minWidth = 50
            }
            tableView.addTableColumn(col)
        }
        tableView.headerView = NSTableHeaderView()
    }

    required init?(coder: NSCoder) { fatalError() }

    /// Replace all rows. Each row must have the same count as columns.
    func setRows(_ newRows: [[String]]) {
        rows = newRows
        tableView.reloadData()
    }
}

extension SimpleTable: NSTableViewDataSource, NSTableViewDelegate {
    func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard let col = tableColumn,
              let idx = tableView.tableColumns.firstIndex(of: col) else { return nil }
        let id = NSUserInterfaceItemIdentifier("cell-\(idx)")
        var view = tableView.makeView(withIdentifier: id, owner: nil) as? NSTextField
        if view == nil {
            view = NSTextField(labelWithString: "")
            view?.identifier = id
            view?.font = .systemFont(ofSize: 11)
            view?.lineBreakMode = .byTruncatingTail
        }
        view?.stringValue = row < rows.count && idx < rows[row].count ? rows[row][idx] : ""
        return view
    }
}
