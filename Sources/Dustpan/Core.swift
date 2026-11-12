import CDustpanCore
import Foundation

// Mirrors the JSON types of core/src/lib.rs.

struct ScanItem: Decodable, Identifiable, Hashable {
    let path: String
    let label: String
    let size: UInt64
    let modified: UInt64?
    let contentsOnly: Bool
    let note: String?
    let suggested: Bool
    var id: String { path }
    var modifiedDate: Date? { modified.map { Date(timeIntervalSince1970: TimeInterval($0)) } }
}

struct ScanCategory: Decodable, Identifiable {
    enum Group: String, Decodable, CaseIterable { case system, ai, developer, projects, review }
    enum Action: String, Decodable { case delete, trash }

    let id: String
    let title: String
    let detail: String
    let group: Group
    let action: Action
    let items: [ScanItem]
    var size: UInt64 { items.reduce(0) { $0 + $1.size } }
}

struct ScanResult: Decodable {
    let categories: [ScanCategory]
    let unreadable: UInt64
}

struct CleanResult: Decodable {
    struct Failure: Decodable { let path: String; let error: String }
    let freed: UInt64
    let removed: Int
    let failed: [Failure]
}

enum Core {
    struct Failure: LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }

    static func scan(home: String, projectRoots: [String], largeFileMB: UInt64) throws -> ScanResult {
        try call(dustpan_scan, [
            "home": home, "project_roots": projectRoots, "large_file_mb": largeFileMB,
        ] as [String: Any])
    }

    static func clean(home: String, projectRoots: [String], items: [ScanItem]) throws -> CleanResult {
        try call(dustpan_clean, [
            "home": home,
            "project_roots": projectRoots,
            "items": items.map { ["path": $0.path, "contents_only": $0.contentsOnly] },
        ] as [String: Any])
    }

    private static func call<T: Decodable>(
        _ fn: (UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?, _ input: [String: Any]
    ) throws -> T {
        let json = String(decoding: try JSONSerialization.data(withJSONObject: input), as: UTF8.self)
        guard let out = json.withCString({ fn($0) }) else { throw Failure(message: "Lõi Rust không trả kết quả") }
        defer { dustpan_free(out) }
        let data = Data(String(cString: out).utf8)
        if let err = try? JSONDecoder().decode([String: String].self, from: data), let msg = err["error"] {
            throw Failure(message: msg)
        }
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(T.self, from: data)
    }
}
