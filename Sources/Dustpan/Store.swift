import AppKit
import Foundation
import Observation

struct DiskInfo {
    let total: Int64
    let available: Int64
    var used: Int64 { total - available }

    static func current() -> DiskInfo? {
        let keys: Set<URLResourceKey> = [.volumeTotalCapacityKey, .volumeAvailableCapacityForImportantUsageKey]
        guard let v = try? URL(fileURLWithPath: "/").resourceValues(forKeys: keys),
              let total = v.volumeTotalCapacity, let available = v.volumeAvailableCapacityForImportantUsage
        else { return nil }
        return DiskInfo(total: Int64(total), available: available)
    }
}

enum CheckState { case off, mixed, on }

func formatBytes<T: BinaryInteger>(_ n: T) -> String {
    ByteCountFormatter.string(fromByteCount: Int64(n), countStyle: .file)
}

@MainActor @Observable
final class AppStore {
    enum Phase { case idle, scanning, cleaning }

    let home = FileManager.default.homeDirectoryForCurrentUser.path
    var phase = Phase.idle
    var result: ScanResult?
    var selected: Set<String> = []
    var error: String?
    var notice: String?
    var disk = DiskInfo.current()

    var projectRoots: [String] {
        didSet { UserDefaults.standard.set(projectRoots, forKey: "projectRoots") }
    }
    var largeFileGB: Int {
        didSet { UserDefaults.standard.set(largeFileGB, forKey: "largeFileGB") }
    }

    init() {
        projectRoots = UserDefaults.standard.stringArray(forKey: "projectRoots") ?? [home]
        let gb = UserDefaults.standard.integer(forKey: "largeFileGB")
        largeFileGB = gb > 0 ? gb : 1
    }

    var categories: [ScanCategory] { result?.categories.filter { !$0.items.isEmpty } ?? [] }

    var selectedItems: [(ScanCategory.Action, ScanItem)] {
        categories.flatMap { c in c.items.filter { selected.contains($0.id) }.map { (c.action, $0) } }
    }

    func selectedSize(_ action: ScanCategory.Action? = nil) -> UInt64 {
        selectedItems.filter { action == nil || $0.0 == action }.reduce(0) { $0 + $1.1.size }
    }

    // MARK: selection

    func state(of c: ScanCategory) -> CheckState {
        let n = c.items.filter { selected.contains($0.id) }.count
        return n == 0 ? .off : n == c.items.count ? .on : .mixed
    }

    func toggle(_ c: ScanCategory) {
        let ids = c.items.map(\.id)
        if state(of: c) == .on { selected.subtract(ids) } else { selected.formUnion(ids) }
    }

    func toggle(_ item: ScanItem) {
        if selected.remove(item.id) == nil { selected.insert(item.id) }
    }

    /// The core decides what is safe to pick by default.
    private static func defaultSelection(_ r: ScanResult) -> Set<String> {
        Set(r.categories.flatMap { $0.items.filter(\.suggested).map(\.id) })
    }

    // MARK: actions

    func scan() async {
        guard phase == .idle else { return }
        phase = .scanning
        error = nil
        let (home, roots, mb) = (home, projectRoots, UInt64(largeFileGB) * 1024)
        do {
            let r = try await Task.detached(priority: .userInitiated) {
                try Core.scan(home: home, projectRoots: roots, largeFileMB: mb)
            }.value
            result = r
            selected = Self.defaultSelection(r)
        } catch {
            self.error = error.localizedDescription
        }
        disk = DiskInfo.current()
        phase = .idle
    }

    func clean() async {
        guard phase == .idle else { return }
        let items = selectedItems
        let toDelete = items.filter { $0.0 == .delete }.map(\.1)
        let toTrash = items.filter { $0.0 == .trash }.map(\.1)
        guard !items.isEmpty else { return }
        phase = .cleaning
        notice = nil
        let (home, roots) = (home, projectRoots)
        do {
            // Delete first: emptying the Trash must not take the items moved there below.
            let deleted = try await Task.detached(priority: .userInitiated) {
                try Core.clean(home: home, projectRoots: roots, items: toDelete)
            }.value
            let trashed = await Task.detached(priority: .userInitiated) { Self.moveToTrash(toTrash) }.value

            var parts = ["Đã xoá \(formatBytes(deleted.freed))"]
            if trashed.moved > 0 { parts.append("chuyển \(formatBytes(trashed.moved)) vào Thùng rác") }
            let failures = deleted.failed.count + trashed.failed
            if failures > 0 { parts.append("\(failures) mục không xoá được") }
            notice = parts.joined(separator: " · ")
        } catch {
            self.error = error.localizedDescription
        }
        phase = .idle
        await scan()
    }

    nonisolated private static func moveToTrash(_ items: [ScanItem]) -> (moved: UInt64, failed: Int) {
        var moved: UInt64 = 0, failed = 0
        let fm = FileManager.default
        for item in items {
            let url = URL(fileURLWithPath: item.path)
            // A folder apps expect to exist (e.g. a model store) is emptied, not moved.
            let targets = item.contentsOnly
                ? (try? fm.contentsOfDirectory(at: url, includingPropertiesForKeys: nil)) ?? []
                : [url]
            var ok = true
            for target in targets {
                do { try fm.trashItem(at: target, resultingItemURL: nil) } catch { ok = false }
            }
            if ok { moved += item.size } else { failed += 1 }
        }
        return (moved, failed)
    }

    func reveal(_ item: ScanItem) {
        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: item.path)])
    }

    func openFullDiskAccess() {
        NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")!)
    }
}
