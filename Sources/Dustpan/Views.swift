import AppKit
import SwiftUI

extension ScanCategory.Group {
    var title: String {
        switch self {
        case .system: "Hệ thống"
        case .ai: "AI"
        case .developer: "Công cụ lập trình"
        case .projects: "Dự án"
        case .review: "Cần xem lại — chuyển vào Thùng rác"
        }
    }
}

struct ContentView: View {
    @Environment(AppStore.self) private var store
    @State private var showSettings = false
    @State private var confirming = false

    var body: some View {
        VStack(spacing: 0) {
            DiskHeader()
            Divider()
            content
            Divider()
            footer
        }
        .frame(minWidth: 680, minHeight: 540)
        .toolbar {
            ToolbarItemGroup {
                Button { Task { await store.scan() } } label: {
                    Label("Quét lại", systemImage: "arrow.clockwise")
                }
                .disabled(store.phase != .idle)
                Button { showSettings = true } label: { Label("Cài đặt", systemImage: "gearshape") }
            }
        }
        .sheet(isPresented: $showSettings) { SettingsView() }
        .task { if store.result == nil { await store.scan() } }
        .confirmationDialog("Dọn dẹp \(formatBytes(store.selectedSize()))?", isPresented: $confirming) {
            Button("Dọn dẹp", role: .destructive) { Task { await store.clean() } }
        } message: {
            Text(confirmMessage)
        }
    }

    private var confirmMessage: String {
        var lines: [String] = []
        let del = store.selectedSize(.delete), trash = store.selectedSize(.trash)
        if del > 0 { lines.append("Xoá vĩnh viễn \(formatBytes(del)) dữ liệu tạo lại được (cache, log, build).") }
        if trash > 0 { lines.append("Chuyển \(formatBytes(trash)) vào Thùng rác — vẫn khôi phục được.") }
        return lines.joined(separator: "\n")
    }

    @ViewBuilder private var content: some View {
        if store.result == nil {
            VStack(spacing: 12) {
                if store.phase == .scanning {
                    ProgressView()
                    Text("Đang quét…").foregroundStyle(.secondary)
                } else if let error = store.error {
                    Text(error).foregroundStyle(.red)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            List {
                if let unreadable = store.result?.unreadable, unreadable > 0 {
                    Section {
                        HStack {
                            Image(systemName: "lock.shield").foregroundStyle(.orange)
                            Text("\(unreadable) thư mục không đọc được. Cấp Full Disk Access cho Dustpan để quét đủ.")
                                .font(.callout)
                            Spacer()
                            Button("Mở cài đặt") { store.openFullDiskAccess() }
                        }
                    }
                }
                ForEach(ScanCategory.Group.allCases, id: \.self) { group in
                    let cats = store.categories.filter { $0.group == group }
                    if !cats.isEmpty {
                        Section(group.title) {
                            ForEach(cats) { CategoryRow(category: $0) }
                        }
                    }
                }
            }
            .listStyle(.inset)
            .overlay {
                if store.phase != .idle {
                    ProgressView(store.phase == .scanning ? "Đang quét…" : "Đang dọn…")
                        .padding(20)
                        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
                }
            }
        }
    }

    private var footer: some View {
        HStack {
            if let notice = store.notice {
                Label(notice, systemImage: "checkmark.circle.fill").foregroundStyle(.green)
            } else if let error = store.error, store.result != nil {
                Text(error).foregroundStyle(.red)
            }
            Spacer()
            Text("Đã chọn \(store.selectedItems.count) mục · \(formatBytes(store.selectedSize()))")
                .monospacedDigit()
                .foregroundStyle(.secondary)
            Button("Dọn dẹp") { confirming = true }
                .keyboardShortcut(.defaultAction)
                .controlSize(.large)
                .disabled(store.phase != .idle || store.selectedItems.isEmpty)
        }
        .padding(12)
    }
}

struct DiskHeader: View {
    @Environment(AppStore.self) private var store

    var body: some View {
        if let disk = store.disk {
            let reclaimable = store.result?.categories.reduce(0) { $0 + $1.size } ?? 0
            VStack(alignment: .leading, spacing: 8) {
                HStack(alignment: .firstTextBaseline) {
                    Text("Macintosh HD").font(.headline)
                    Spacer()
                    Text("Đã dùng \(formatBytes(disk.used)) / \(formatBytes(disk.total)) · trống \(formatBytes(disk.available))")
                        .monospacedDigit()
                        .foregroundStyle(.secondary)
                }
                GeometryReader { geo in
                    let w = geo.size.width
                    let used = CGFloat(disk.used) / CGFloat(max(disk.total, 1))
                    let free = min(CGFloat(reclaimable) / CGFloat(max(disk.total, 1)), used)
                    ZStack(alignment: .leading) {
                        Capsule().fill(.quaternary)
                        Capsule().fill(Color.accentColor).frame(width: w * used)
                        Rectangle().fill(.green)
                            .frame(width: w * free)
                            .offset(x: w * (used - free))
                    }
                    .clipShape(Capsule())
                }
                .frame(height: 10)
                if reclaimable > 0 {
                    Label("Có thể giải phóng khoảng \(formatBytes(reclaimable))", systemImage: "leaf")
                        .font(.callout)
                        .foregroundStyle(.green)
                }
            }
            .padding(16)
        }
    }
}

struct CheckBox: View {
    let state: CheckState
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: state == .on ? "checkmark.square.fill" : state == .mixed ? "minus.square.fill" : "square")
                .font(.title3)
                .foregroundStyle(state == .off ? AnyShapeStyle(.secondary) : AnyShapeStyle(Color.accentColor))
        }
        .buttonStyle(.plain)
    }
}

struct CategoryRow: View {
    @Environment(AppStore.self) private var store
    let category: ScanCategory

    var body: some View {
        DisclosureGroup {
            ForEach(category.items) { ItemRow(item: $0) }
        } label: {
            HStack(spacing: 10) {
                CheckBox(state: store.state(of: category)) { store.toggle(category) }
                VStack(alignment: .leading, spacing: 2) {
                    Text(category.title).fontWeight(.medium)
                    Text(category.detail).font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
                Text(formatBytes(category.size)).monospacedDigit().fontWeight(.medium)
            }
            .padding(.vertical, 4)
        }
    }
}

struct ItemRow: View {
    @Environment(AppStore.self) private var store
    let item: ScanItem

    var body: some View {
        HStack(spacing: 10) {
            CheckBox(state: store.selected.contains(item.id) ? .on : .off) { store.toggle(item) }
            VStack(alignment: .leading, spacing: 1) {
                Text(item.label).lineLimit(1)
                if let note = item.note {
                    Text(note).font(.caption).foregroundStyle(.orange).lineLimit(1).truncationMode(.middle)
                }
                Text(item.path).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
            }
            Spacer()
            if let date = item.modifiedDate {
                Text(date, format: .relative(presentation: .named))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Text(formatBytes(item.size)).monospacedDigit().frame(minWidth: 70, alignment: .trailing)
        }
        .contextMenu {
            Button("Hiện trong Finder") { store.reveal(item) }
        }
    }
}

struct SettingsView: View {
    @Environment(AppStore.self) private var store
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        @Bindable var store = store
        VStack(alignment: .leading, spacing: 16) {
            Text("Cài đặt").font(.title2.bold())
            VStack(alignment: .leading, spacing: 6) {
                Text("Thư mục tìm dự án và tệp lớn").font(.headline)
                Text("Tìm node_modules, target, .build, Pods… và tệp vượt ngưỡng trong các thư mục này.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                List {
                    ForEach(store.projectRoots, id: \.self) { root in
                        HStack {
                            Image(systemName: "folder")
                            Text(root).lineLimit(1).truncationMode(.middle)
                            Spacer()
                            Button { store.projectRoots.removeAll { $0 == root } } label: {
                                Image(systemName: "minus.circle")
                            }
                            .buttonStyle(.borderless)
                        }
                    }
                }
                .frame(height: 140)
                Button("Thêm thư mục…", action: addRoot)
            }
            Stepper("Tệp lớn từ \(store.largeFileGB) GB", value: $store.largeFileGB, in: 1...100)
            HStack {
                Spacer()
                Button("Xong") {
                    dismiss()
                    Task { await store.scan() }
                }
                .keyboardShortcut(.defaultAction)
            }
        }
        .padding(20)
        .frame(width: 480)
    }

    private func addRoot() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = true
        guard panel.runModal() == .OK else { return }
        for url in panel.urls where !store.projectRoots.contains(url.path) {
            store.projectRoots.append(url.path)
        }
    }
}
