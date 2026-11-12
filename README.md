# Dustpan

App macOS dọn dữ liệu không cần thiết để giải phóng dung lượng. Lõi quét/xoá viết bằng Rust
(`core/`, build thành static lib), giao diện SwiftUI (`Sources/Dustpan`) gọi lõi qua C FFI bằng JSON.

## Build

```sh
./build.sh            # chạy test Rust, build release, tạo build/Dustpan.app
open build/Dustpan.app
```

Xem trước những gì sẽ được liệt kê (chỉ đọc, không xoá):

```sh
cargo run --release --manifest-path core/Cargo.toml --example scan [thư-mục-dự-án…]
```

## Dọn những gì

| Nhóm | Mục | Cách xoá | Chọn sẵn |
|---|---|---|---|
| Hệ thống | ~/Library/Caches, ~/.cache, ~/Library/Logs, crash/heap dump (Crashpad, .hprof, .heapsnapshot, core.<pid>, hs_err_pid, npm-debug.log — kể cả tệp ẩn), tệp đính kèm Mail, Thùng rác | xoá vĩnh viễn | có |
| AI | cache của Claude, VS Code, Cursor, Windsurf, Antigravity, Codex, Gemini CLI; `.playwright-mcp`, cache aider trong dự án | xoá vĩnh viễn | có |
| AI | lịch sử phiên Claude Code / Codex | Thùng rác | phiên bỏ hơn 30 ngày |
| AI | model đã tải (HuggingFace, Ollama, LM Studio, máy ảo Claude) | Thùng rác | không |
| Công cụ lập trình | Xcode DerivedData / Device Support, cache Simulator, cache npm/pnpm/Yarn/Bun/Cargo/Go/Gradle/pip/CocoaPods/Homebrew/Playwright… | xoá vĩnh viễn | có |
| Dự án | node_modules, target, .build, Pods, .next, .dart_tool, build/.gradle (chỉ khi có tệp dự án tương ứng bên cạnh) | xoá vĩnh viễn | dự án bỏ hơn 30 ngày |
| Cần xem lại | từng mục trong Downloads, kèm lý do | Thùng rác | bộ cài, tải dở, tệp nén đã giải nén, bản trùng, bản cũ trong chuỗi bản sao lưu có ngày giờ (giữ bản mới nhất), mục để quá 90 ngày |
| Cần xem lại | tệp trùng lặp (giống từng byte, ≥ 1 MB, ngoài repo/dự án/SDK) | Thùng rác | bản trong Downloads hoặc tên kiểu “(1)”, “copy” |
| Cần xem lại | tệp lớn, Xcode Archives, backup iPhone | Thùng rác | không |

Dung lượng tệp là phần thực sự được giải phóng: bản clone APFS dùng chung block nên không tính
(và bản trùng là clone hoàn toàn thì không liệt kê). Mỗi đường dẫn chỉ xuất hiện ở một mục.

Thư mục tìm dự án/trùng lặp/tệp lớn và ngưỡng tệp lớn chỉnh trong Cài đặt (mặc định: home + Downloads, 1 GB).

## Icon & DMG

```sh
swift scripts/make-icon.swift   # vẽ lại Resources/AppIcon.icns + AppIcon.png
./dmg.sh                        # build/Dustpan-<version>.dmg (app + lối tắt Applications)
```

## An toàn

- Lõi Rust chỉ xoá đường dẫn nằm hẳn bên trong home hoặc thư mục dự án đã chọn, từ chối symlink,
  `..`, và các thư mục được bảo vệ (home, Documents, Library, Library/Caches…).
- Không đi theo symlink khi quét hay xoá; hard link chỉ tính dung lượng một lần.
- Cache/log được xoá trước, sau đó mới chuyển mục "cần xem lại" vào Thùng rác, để việc dọn
  Thùng rác không xoá luôn chúng.
- Muốn quét cả thư mục hệ thống được bảo vệ (Mail, Containers…), cấp Full Disk Access cho Dustpan.
