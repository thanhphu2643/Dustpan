# Dustpan

A macOS app that finds the data you no longer need (caches, logs, AI tool leftovers, dumps,
build output, duplicates, stale downloads) and frees the space safely.

The scanner and the deleter are written in Rust (`core/`, built as a static library); the
SwiftUI app (`Sources/Dustpan`) calls it through a small C interface that exchanges JSON.
The interface text is in Vietnamese.

## Build

Requires macOS 14+, Xcode command line tools and a Rust toolchain.

```sh
./build.sh            # runs the Rust tests, builds release, creates build/Dustpan.app
open build/Dustpan.app
./dmg.sh              # build/Dustpan-<version>.dmg with an Applications shortcut
```

Preview what the app would list, without deleting anything:

```sh
cargo run --release --manifest-path core/Cargo.toml --example scan [project-folder…]
```

Redraw the icon with `swift scripts/make-icon.swift`.

## What it cleans

| Group | Items | Removal | Selected by default |
|---|---|---|---|
| System | ~/Library/Caches, ~/.cache, ~/Library/Logs, crash and heap dumps (Crashpad, .hprof, .heapsnapshot, core.&lt;pid&gt;, hs_err_pid, npm-debug.log, hidden files included), opened Mail attachments, the Trash | deleted | yes |
| AI | caches of Claude, VS Code, Cursor, Windsurf, Antigravity, Codex, Gemini CLI; `.playwright-mcp` and aider caches inside projects | deleted | yes |
| AI | Claude Code / Codex session history | Trash | sessions idle for 30+ days |
| AI | downloaded models (Hugging Face, Ollama, LM Studio, Claude VM) | Trash | no |
| Developer tools | Xcode DerivedData / Device Support, Simulator caches, npm/pnpm/Yarn/Bun/Cargo/Go/Gradle/pip/CocoaPods/Homebrew/Playwright caches | deleted | yes |
| Projects | node_modules, target, .build, Pods, .next, .dart_tool, build/.gradle (only next to the matching project file) | deleted | projects idle for 30+ days |
| Review | every item in Downloads, with the reason it may go | Trash | installers, partial downloads, extracted archives, duplicates, older copies in a dated backup series (newest kept), items untouched for 90+ days |
| Review | duplicate files (byte-identical, ≥ 1 MB, outside repositories, projects and SDKs) | Trash | copies in Downloads or named like “(1)” or “copy” |
| Review | large files, Xcode Archives, iPhone backups | Trash | no |

File sizes are the space actually freed: blocks shared with an APFS clone are not counted, and
a duplicate that is a full clone is not listed. Each path appears under one item only.

The folders searched for projects, duplicates and large files, and the large-file threshold,
are set in Settings (default: home and Downloads, 1 GB).

## Safety

- The Rust core only deletes paths strictly inside the home folder or a chosen folder, and
  refuses symlinks, `..` and protected folders (home, Documents, Library, Library/Caches…).
- Scanning and deleting never follow symlinks; hard links are counted once.
- Caches and logs are deleted first, then review items go to the Trash, so emptying the Trash
  in the same run never removes them.
- Grant Dustpan Full Disk Access to scan protected folders such as Mail and app containers.
