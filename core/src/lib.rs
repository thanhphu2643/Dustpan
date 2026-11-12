//! Finds disk space that can be reclaimed (caches, logs, AI tool leftovers, dumps, build
//! output, duplicates, stale downloads) and deletes it safely. The Swift app talks to this
//! crate through three C functions that take and return JSON strings.

use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::{c_char, c_void, CStr, CString};
use std::fs;
use std::hash::Hasher;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

// ---------- data shared with Swift ----------

#[derive(Deserialize)]
pub struct ScanConfig {
    pub home: PathBuf,
    /// Folders searched for project build output, dumps, duplicates and large files.
    #[serde(default)]
    pub project_roots: Vec<PathBuf>,
    #[serde(default = "default_large_file_mb")]
    pub large_file_mb: u64,
}

fn default_large_file_mb() -> u64 {
    1024
}

#[derive(Serialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Group {
    System,
    Developer,
    Ai,
    Projects,
    Review,
}

/// How the app removes the items of a category: regenerable data is deleted,
/// anything a person might still want goes to the Trash.
#[derive(Serialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Delete,
    Trash,
}

#[derive(Serialize, Debug, Clone, Default)]
pub struct Item {
    pub path: PathBuf,
    pub label: String,
    /// Bytes freed by removing it (allocated size; for files, the part not shared with clones).
    pub size: u64,
    /// Last modification, seconds since 1970.
    pub modified: Option<u64>,
    /// Empty the folder but keep it (for folders apps expect to exist).
    pub contents_only: bool,
    /// Why it is listed, shown under the name.
    pub note: Option<String>,
    /// Selected by default in the app.
    pub suggested: bool,
}

#[derive(Serialize, Debug)]
pub struct Category {
    pub id: &'static str,
    pub title: &'static str,
    pub detail: &'static str,
    pub group: Group,
    pub action: Action,
    pub items: Vec<Item>,
}

#[derive(Serialize, Debug)]
pub struct ScanResult {
    pub categories: Vec<Category>,
    /// Files or folders that could not be read (usually missing Full Disk Access).
    pub unreadable: u64,
}

#[derive(Deserialize)]
pub struct CleanRequest {
    pub home: PathBuf,
    #[serde(default)]
    pub project_roots: Vec<PathBuf>,
    pub items: Vec<CleanItem>,
}

#[derive(Deserialize)]
pub struct CleanItem {
    pub path: PathBuf,
    #[serde(default)]
    pub contents_only: bool,
}

#[derive(Serialize, Debug, Default)]
pub struct CleanResult {
    pub freed: u64,
    pub removed: usize,
    pub failed: Vec<Failure>,
}

#[derive(Serialize, Debug)]
pub struct Failure {
    pub path: PathBuf,
    pub error: String,
}

// ---------- what to look for ----------

enum Source {
    /// Every entry inside these folders (relative to home) is one item.
    Children(&'static [&'static str]),
    /// Each existing folder (relative to home) is one item whose contents get emptied.
    Folders(&'static [(&'static str, &'static str)]),
    /// `Library/Application Support/<app>/<sub>` for every app × sub that exists, emptied in place.
    AppFolders(&'static [&'static str], &'static [&'static str]),
    /// Electron/Chromium crash reports under Library/Application Support.
    Crashpad,
    /// Top-level entries of ~/Downloads, each with the reason it may go.
    Downloads,
    /// Filled from the walk over the project roots.
    Walk(Found),
}

#[derive(Clone, Copy, PartialEq)]
enum Found {
    ProjectJunk,
    AiFiles,
    Dumps,
    Duplicates,
    LargeFiles,
}

/// Which items start out selected.
#[derive(Clone, Copy)]
enum Suggest {
    All,
    /// Untouched for more than this many days.
    OlderThan(u64),
    /// Each item says so itself (or nothing is selected).
    PerItem,
}

struct Spec {
    id: &'static str,
    title: &'static str,
    detail: &'static str,
    group: Group,
    action: Action,
    suggest: Suggest,
    sources: &'static [Source],
}

const PACKAGE_CACHES: &[(&str, &str)] = &[
    (".npm/_cacache", "npm"),
    (".npm/_logs", "npm logs"),
    ("Library/Caches/Yarn", "Yarn"),
    ("Library/pnpm/store", "pnpm store"),
    ("Library/Caches/pnpm", "pnpm"),
    (".bun/install/cache", "Bun"),
    ("Library/Caches/node-gyp", "node-gyp"),
    ("Library/Caches/ms-playwright", "Playwright browsers"),
    (".cargo/registry/cache", "Cargo registry (.crate)"),
    (".cargo/registry/src", "Cargo registry (src)"),
    (".cargo/git/checkouts", "Cargo git checkouts"),
    ("Library/Caches/go-build", "Go build cache"),
    ("go/pkg/mod", "Go modules"),
    (".gradle/caches", "Gradle"),
    ("Library/Caches/CocoaPods", "CocoaPods"),
    ("Library/Caches/pip", "pip"),
    ("Library/Caches/pypoetry", "Poetry"),
    ("Library/Caches/Homebrew", "Homebrew downloads"),
    (".pub-cache/hosted", "Dart/Flutter pub"),
    ("Library/Caches/org.swift.swiftpm", "SwiftPM"),
];

/// Electron-based AI apps and editors keep regenerable caches next to their real data.
const AI_APPS: &[&str] = &[
    "Claude", "Code", "Cursor", "Windsurf", "Antigravity", "Antigravity IDE", "Kiro", "Trae",
    "Code - Insiders", "VSCodium",
];
const ELECTRON_CACHES: &[&str] = &[
    "Cache", "Code Cache", "GPUCache", "DawnGraphiteCache", "DawnWebGPUCache", "CachedData",
    "CachedExtensionVSIXs", "CachedProfilesData", "logs", "Service Worker/CacheStorage",
];
const AI_CLI_CACHES: &[(&str, &str)] = &[
    (".claude/shell-snapshots", "Claude Code · shell snapshots"),
    (".claude/paste-cache", "Claude Code · paste cache"),
    (".claude/debug", "Claude Code · debug logs"),
    (".codex/cache", "Codex · cache"),
    (".codex/shell_snapshots", "Codex · shell snapshots"),
    (".codex/log", "Codex · logs"),
    (".gemini/tmp", "Gemini CLI · tmp"),
    (".cache/chrome-devtools-mcp", "Chrome DevTools MCP"),
];
const AI_MODELS: &[(&str, &str)] = &[
    (".ollama/models", "Ollama models"),
    ("Library/Application Support/Claude/vm_bundles", "Claude · máy ảo"),
];

const SPECS: &[Spec] = &[
    Spec {
        id: "app_caches",
        title: "Bộ nhớ đệm ứng dụng",
        detail: "~/Library/Caches — ứng dụng tự tạo lại khi cần.",
        group: Group::System,
        action: Action::Delete,
        suggest: Suggest::All,
        sources: &[Source::Children(&["Library/Caches"])],
    },
    Spec {
        id: "hidden_caches",
        title: "Cache ẩn (~/.cache)",
        detail: "Thư mục cache ẩn của công cụ dòng lệnh.",
        group: Group::System,
        action: Action::Delete,
        suggest: Suggest::All,
        sources: &[Source::Children(&[".cache"])],
    },
    Spec {
        id: "logs",
        title: "Nhật ký (log)",
        detail: "~/Library/Logs — log cũ của ứng dụng và báo cáo lỗi.",
        group: Group::System,
        action: Action::Delete,
        suggest: Suggest::All,
        sources: &[Source::Children(&["Library/Logs"])],
    },
    Spec {
        id: "dumps",
        title: "Crash dump & heap dump",
        detail: "Crashpad, .hprof, .heapsnapshot, core.<pid>, hs_err_pid, npm-debug.log… kể cả tệp ẩn.",
        group: Group::System,
        action: Action::Delete,
        suggest: Suggest::All,
        sources: &[Source::Crashpad, Source::Walk(Found::Dumps)],
    },
    Spec {
        id: "mail_downloads",
        title: "Tệp đính kèm Mail đã mở",
        detail: "Bản sao tệp đính kèm Mail lưu khi bạn mở; thư gốc vẫn còn.",
        group: Group::System,
        action: Action::Delete,
        suggest: Suggest::All,
        sources: &[Source::Folders(&[(
            "Library/Containers/com.apple.mail/Data/Library/Mail Downloads",
            "Mail Downloads",
        )])],
    },
    Spec {
        id: "trash",
        title: "Thùng rác",
        detail: "Xoá vĩnh viễn những gì đang trong Thùng rác.",
        group: Group::System,
        action: Action::Delete,
        suggest: Suggest::All,
        sources: &[Source::Folders(&[(".Trash", "Thùng rác")])],
    },
    Spec {
        id: "ai_caches",
        title: "Cache của app AI & editor",
        detail: "Claude, VS Code, Cursor, Windsurf, Antigravity, Codex, Gemini CLI… — tự tạo lại.",
        group: Group::Ai,
        action: Action::Delete,
        suggest: Suggest::All,
        sources: &[Source::AppFolders(AI_APPS, ELECTRON_CACHES), Source::Folders(AI_CLI_CACHES)],
    },
    Spec {
        id: "ai_project_files",
        title: "Tệp AI sinh ra trong dự án",
        detail: "Ảnh chụp/log của Playwright MCP (.playwright-mcp), cache của aider.",
        group: Group::Ai,
        action: Action::Delete,
        suggest: Suggest::All,
        sources: &[Source::Walk(Found::AiFiles)],
    },
    Spec {
        id: "ai_history",
        title: "Lịch sử phiên AI agent",
        detail: "Transcript Claude Code / Codex theo dự án; xoá thì không resume được phiên cũ.",
        group: Group::Ai,
        action: Action::Trash,
        suggest: Suggest::OlderThan(30),
        sources: &[Source::Children(&[".claude/projects", ".codex/sessions"])],
    },
    Spec {
        id: "ai_models",
        title: "Model AI đã tải",
        detail: "HuggingFace, Ollama, LM Studio, máy ảo Claude — tải lại được nhưng rất lâu.",
        group: Group::Ai,
        action: Action::Trash,
        suggest: Suggest::PerItem,
        sources: &[
            Source::Children(&[".cache/huggingface/hub", ".cache/lm-studio/models", ".lmstudio/models"]),
            Source::Folders(AI_MODELS),
        ],
    },
    Spec {
        id: "xcode_derived_data",
        title: "Xcode DerivedData",
        detail: "Kết quả build và index của Xcode; build lại sẽ tạo lại.",
        group: Group::Developer,
        action: Action::Delete,
        suggest: Suggest::All,
        sources: &[Source::Children(&["Library/Developer/Xcode/DerivedData"])],
    },
    Spec {
        id: "xcode_device_support",
        title: "Xcode Device Support",
        detail: "Symbol của các phiên bản iOS/watchOS đã từng cắm máy; Xcode tải lại khi cần.",
        group: Group::Developer,
        action: Action::Delete,
        suggest: Suggest::All,
        sources: &[Source::Children(&[
            "Library/Developer/Xcode/iOS DeviceSupport",
            "Library/Developer/Xcode/watchOS DeviceSupport",
            "Library/Developer/Xcode/tvOS DeviceSupport",
            "Library/Developer/Xcode/visionOS DeviceSupport",
        ])],
    },
    Spec {
        id: "simulator_caches",
        title: "Cache Simulator",
        detail: "Bộ nhớ đệm của CoreSimulator.",
        group: Group::Developer,
        action: Action::Delete,
        suggest: Suggest::All,
        sources: &[Source::Folders(&[("Library/Developer/CoreSimulator/Caches", "CoreSimulator")])],
    },
    Spec {
        id: "package_caches",
        title: "Cache trình quản lý gói",
        detail: "npm, pnpm, Cargo, Go, Gradle, pip, Homebrew… — tải lại khi cài.",
        group: Group::Developer,
        action: Action::Delete,
        suggest: Suggest::All,
        sources: &[Source::Folders(PACKAGE_CACHES)],
    },
    Spec {
        id: "project_junk",
        title: "Thư mục build trong dự án",
        detail: "node_modules, target, .build, Pods… — cài/build lại là có. Chọn sẵn dự án bỏ hơn 30 ngày.",
        group: Group::Projects,
        action: Action::Delete,
        suggest: Suggest::OlderThan(30),
        sources: &[Source::Walk(Found::ProjectJunk)],
    },
    Spec {
        id: "downloads",
        title: "Thư mục Downloads",
        detail: "Chọn sẵn: bộ cài, tải dở, tệp nén đã giải nén, bản trùng, mục để quá 90 ngày.",
        group: Group::Review,
        action: Action::Trash,
        suggest: Suggest::PerItem,
        sources: &[Source::Downloads],
    },
    Spec {
        id: "duplicates",
        title: "Tệp trùng lặp",
        detail: "Nội dung giống hệt từng byte; giữ lại một bản. Chọn sẵn bản nằm trong Downloads hoặc tên kiểu “(1)”, “copy”.",
        group: Group::Review,
        action: Action::Trash,
        suggest: Suggest::PerItem,
        sources: &[Source::Walk(Found::Duplicates)],
    },
    Spec {
        id: "large_files",
        title: "Tệp lớn",
        detail: "Tệp vượt ngưỡng trong thư mục đã chọn.",
        group: Group::Review,
        action: Action::Trash,
        suggest: Suggest::PerItem,
        sources: &[Source::Walk(Found::LargeFiles)],
    },
    Spec {
        id: "xcode_archives",
        title: "Xcode Archives",
        detail: "Bản archive đã phát hành; cần giữ nếu còn dùng dSYM.",
        group: Group::Review,
        action: Action::Trash,
        suggest: Suggest::PerItem,
        sources: &[Source::Children(&["Library/Developer/Xcode/Archives"])],
    },
    Spec {
        id: "ios_backups",
        title: "Bản sao lưu iPhone/iPad",
        detail: "Backup thiết bị trong Finder.",
        group: Group::Review,
        action: Action::Trash,
        suggest: Suggest::PerItem,
        sources: &[Source::Children(&["Library/Application Support/MobileSync/Backup"])],
    },
];

/// A build-output folder name and the files that prove it sits in a project.
const JUNK_DIRS: &[(&str, &[&str])] = &[
    ("node_modules", &["package.json"]),
    (".next", &["package.json"]),
    (".nuxt", &["package.json"]),
    (".turbo", &["package.json"]),
    (".parcel-cache", &["package.json"]),
    (".angular", &["angular.json"]),
    ("target", &["Cargo.toml"]),
    (".build", &["Package.swift"]),
    ("Pods", &["Podfile"]),
    (".dart_tool", &["pubspec.yaml"]),
    ("build", &["pubspec.yaml", "build.gradle", "build.gradle.kts"]),
    (".gradle", &["build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts"]),
];

/// Files that mark a folder as a repository, project or SDK.
const PROJECT_MARKERS: &[&str] = &[
    ".git", "package.json", "Cargo.toml", "go.mod", "pubspec.yaml", "Package.swift", "build.gradle",
    "build.gradle.kts", "pom.xml", "pyproject.toml", "setup.py", "Gemfile", "composer.json", "Makefile",
    "CMakeLists.txt", "Podfile", "Info.plist",
];

/// Folders that AI tools drop into whatever directory they ran in.
const AI_DIRS: &[&str] = &[".playwright-mcp", ".aider.tags.cache.v3"];

/// Folder bundles whose insides must never be listed as separate files.
const PACKAGE_EXTS: &[&str] = &[
    "app", "photoslibrary", "musiclibrary", "tvlibrary", "imovielibrary", "fcpbundle",
    "logicx", "bundle", "framework", "xcarchive", "sparsebundle", "git",
];

const INSTALLER_EXTS: &[&str] = &["dmg", "pkg", "mpkg", "xip", "iso"];
const PARTIAL_EXTS: &[&str] = &["crdownload", "part", "partial", "download", "opdownload"];
const ARCHIVE_EXTS: &[&str] = &[".tar.gz", ".tar.xz", ".tar.bz2", ".tgz", ".tar", ".zip", ".rar", ".7z"];

const MAX_WALK_DEPTH: usize = 12;
const DUPLICATE_MIN_BYTES: u64 = 1024 * 1024;
const STALE_DOWNLOAD_DAYS: u64 = 90;

// ---------- scanning ----------

pub fn scan(cfg: &ScanConfig) -> ScanResult {
    let home = &cfg.home;
    let downloads = home.join("Downloads");
    let mut walk_roots = cfg.project_roots.clone();
    if walk_roots.is_empty() {
        walk_roots.push(home.clone());
    }
    walk_roots.push(downloads.clone()); // duplicates and dumps in Downloads, whatever the roots
    let roots = normalize_roots(&walk_roots);
    let large_min = cfg.large_file_mb.saturating_mul(1024 * 1024).max(1);

    let (mut walked, mut categories) = std::thread::scope(|s| {
        let walker = s.spawn(|| {
            let mut w = walk(&roots, home, large_min);
            w.duplicates = find_duplicates(std::mem::take(&mut w.candidates), &downloads);
            w
        });
        let categories: Vec<Category> = SPECS
            .iter()
            .enumerate()
            .map(|(i, spec)| Category {
                id: spec.id,
                title: spec.title,
                detail: spec.detail,
                group: spec.group,
                action: spec.action,
                items: spec.sources.iter().flat_map(|src| collect_items(src, i, home)).collect(),
            })
            .collect();
        (walker.join().unwrap_or_default(), categories)
    });

    // Hand the walk results to their categories; duplicates found in Downloads stay there.
    let dup_of: HashMap<PathBuf, PathBuf> =
        walked.duplicates.iter().map(|(extra, kept)| (extra.path.clone(), kept.clone())).collect();
    for (spec, cat) in SPECS.iter().zip(categories.iter_mut()) {
        for src in spec.sources {
            if let Source::Walk(found) = src {
                cat.items.extend(match found {
                    Found::ProjectJunk => std::mem::take(&mut walked.junk),
                    Found::AiFiles => std::mem::take(&mut walked.ai),
                    Found::Dumps => std::mem::take(&mut walked.dumps),
                    Found::LargeFiles => std::mem::take(&mut walked.large),
                    Found::Duplicates => std::mem::take(&mut walked.duplicates).into_iter().map(|(it, _)| it).collect(),
                });
            }
        }
        if cat.id == "downloads" {
            for it in &mut cat.items {
                if let Some(kept) = dup_of.get(&it.path) {
                    add_note(it, &format!("trùng với {}", tilde(kept, home)));
                    it.suggested = true;
                }
            }
        }
    }
    dedupe_paths(&mut categories);

    // Measure everything in parallel; items that already know their size are skipped.
    let pending: Vec<(usize, usize)> = categories
        .iter()
        .enumerate()
        .flat_map(|(c, cat)| (0..cat.items.len()).map(move |i| (c, i)))
        .filter(|&(c, i)| categories[c].items[i].size == 0)
        .collect();
    let sizes = Mutex::new(vec![(0u64, 0u64); pending.len()]);
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let k = next.fetch_add(1, Ordering::Relaxed);
                let Some(&(c, i)) = pending.get(k) else { break };
                let measured = disk_usage(&categories[c].items[i].path);
                sizes.lock().unwrap()[k] = measured;
            });
        }
    });
    let mut unreadable = walked.unreadable;
    for (k, (size, errs)) in sizes.into_inner().unwrap().into_iter().enumerate() {
        let (c, i) = pending[k];
        categories[c].items[i].size = size;
        unreadable += errs;
    }

    let now = now_secs();
    for (spec, cat) in SPECS.iter().zip(categories.iter_mut()) {
        cat.items.retain(|it| it.size > 0);
        cat.items.sort_by(|a, b| b.size.cmp(&a.size));
        for it in &mut cat.items {
            match spec.suggest {
                Suggest::All => it.suggested = true,
                Suggest::OlderThan(days) => it.suggested = it.modified.is_some_and(|m| now.saturating_sub(m) > days * 86400),
                Suggest::PerItem => {}
            }
        }
    }
    ScanResult { categories, unreadable }
}

/// Each path is listed once, and never together with a folder that contains it;
/// the earlier category wins.
fn dedupe_paths(categories: &mut [Category]) {
    let mut kept: BTreeSet<PathBuf> = BTreeSet::new();
    for cat in categories {
        cat.items.retain(|it| {
            let p = &it.path;
            let under_kept = p.ancestors().any(|a| kept.contains(a));
            // Descendants sort right after their ancestor in Path order.
            let holds_kept = kept.range(p.clone()..).next().is_some_and(|k| k.starts_with(p));
            if under_kept || holds_kept {
                return false;
            }
            kept.insert(p.clone());
            true
        });
    }
}

/// Folders that another spec lists on purpose: generic children lists skip them,
/// along with anything containing or inside them.
fn owned_elsewhere(spec_index: usize, home: &Path) -> Vec<PathBuf> {
    SPECS
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != spec_index)
        .flat_map(|(_, s)| s.sources.iter())
        .flat_map(|src| -> Vec<PathBuf> {
            match *src {
                Source::Children(dirs) => dirs.iter().map(|d| home.join(d)).collect(),
                Source::Folders(list) => list.iter().map(|(d, _)| home.join(d)).collect(),
                _ => Vec::new(),
            }
        })
        .collect()
}

fn collect_items(source: &Source, spec_index: usize, home: &Path) -> Vec<Item> {
    match *source {
        Source::Children(dirs) => {
            let owned = owned_elsewhere(spec_index, home);
            let mut out = Vec::new();
            for rel in dirs {
                let root = home.join(rel);
                let Ok(rd) = fs::read_dir(&root) else { continue };
                for e in rd.flatten() {
                    let p = e.path();
                    // Skip a child that holds another category's folder, or sits in one
                    // more specific than this root (a broader root like ~/.cache does not count).
                    let taken = owned.iter().any(|o| o.starts_with(&p) || (p.starts_with(o) && o.starts_with(&root) && *o != root));
                    if e.file_name() != ".DS_Store" && !taken {
                        out.push(item(p, e.file_name().to_string_lossy().into_owned(), false));
                    }
                }
            }
            out
        }
        Source::Folders(list) => list
            .iter()
            .map(|(rel, label)| (home.join(rel), label))
            .filter(|(p, _)| is_real_dir(p))
            .map(|(p, label)| item(p, label.to_string(), true))
            .collect(),
        Source::AppFolders(apps, subs) => {
            let support = home.join("Library/Application Support");
            apps.iter()
                .flat_map(|app| subs.iter().map(move |sub| (app, sub)))
                .map(|(app, sub)| (support.join(app).join(sub), format!("{app} · {sub}")))
                .filter(|(p, _)| is_real_dir(p))
                .map(|(p, label)| item(p, label, true))
                .collect()
        }
        Source::Crashpad => crashpad_items(home),
        Source::Downloads => download_items(home),
        Source::Walk(_) => Vec::new(),
    }
}

fn crashpad_items(home: &Path) -> Vec<Item> {
    let support = home.join("Library/Application Support");
    let children = |p: &Path| -> Vec<PathBuf> {
        fs::read_dir(p).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default()
    };
    // Crashpad sits at <app>/Crashpad or one level deeper (Google/Chrome/Crashpad).
    let apps: Vec<PathBuf> = children(&support)
        .into_iter()
        .flat_map(|app| {
            let mut v = children(&app);
            v.push(app);
            v
        })
        .collect();
    let mut out = Vec::new();
    for app in apps {
        for sub in ["completed", "pending", "new"] {
            let p = app.join("Crashpad").join(sub);
            if is_real_dir(&p) {
                let name = app.strip_prefix(&support).unwrap_or(&app).display().to_string();
                out.push(item(p, format!("{name} · Crashpad/{sub}"), true));
            }
        }
    }
    out
}

fn download_items(home: &Path) -> Vec<Item> {
    let dir = home.join("Downloads");
    let Ok(rd) = fs::read_dir(&dir) else { return Vec::new() };
    let entries: Vec<fs::DirEntry> = rd.flatten().collect();
    let dirs: HashSet<String> = entries
        .iter()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let now = now_secs();
    let mut out = Vec::new();
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        if name == ".DS_Store" || name == ".localized" {
            continue;
        }
        let mut it = item(e.path(), name.clone(), false);
        let lower = name.to_lowercase();
        let ext = lower.rsplit_once('.').map_or("", |(_, x)| x);
        if INSTALLER_EXTS.contains(&ext) {
            add_note(&mut it, "bộ cài");
            it.suggested = true;
        }
        if PARTIAL_EXTS.contains(&ext) {
            add_note(&mut it, "tải dở");
            it.suggested = true;
        }
        if let Some(stem) = ARCHIVE_EXTS.iter().find_map(|x| lower.ends_with(x).then(|| &name[..name.len() - x.len()])) {
            if dirs.contains(stem) {
                add_note(&mut it, &format!("đã giải nén → {stem}/"));
                it.suggested = true;
            }
        }
        let touched = fs::symlink_metadata(e.path()).ok().map(|m| {
            let created = m.created().ok().and_then(secs);
            created.max(mtime(&m)).unwrap_or(0)
        });
        if let Some(t) = touched {
            let days = now.saturating_sub(t) / 86400;
            if t > 0 && days > STALE_DOWNLOAD_DAYS {
                add_note(&mut it, &format!("để {} tháng không đụng tới", days / 30));
                it.suggested = true;
            }
        }
        out.push(it);
    }
    mark_old_versions(&mut out);
    out
}

/// "theciu_2026-10-19T07-35-04-682Z", "db_2026-03-17_03-46-03.dump": the part before the ISO
/// date, the date-time run, and what follows it.
fn split_timestamp(name: &str) -> Option<(&str, &str, &str)> {
    let b = name.as_bytes();
    let d = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
    let start = (1..b.len()).find(|&i| {
        b[i] == b'2' && b[i + 1..].starts_with(b"0") && d(i + 2) && d(i + 3) && b.get(i + 4) == Some(&b'-')
            && d(i + 5) && d(i + 6) && b.get(i + 7) == Some(&b'-') && d(i + 8) && d(i + 9)
    })?;
    let len = b[start..]
        .iter()
        .take_while(|c| c.is_ascii_digit() || matches!(c, b'-' | b'_' | b'T' | b'Z' | b':'))
        .count();
    Some((&name[..start], &name[start..start + len], &name[start + len..]))
}

/// In a run of dated copies of the same thing (same prefix, same kind) only the newest stays.
fn mark_old_versions(items: &mut [Item]) {
    let mut series: HashMap<(String, String, bool), Vec<(String, usize)>> = HashMap::new();
    for (i, it) in items.iter().enumerate() {
        if let Some((prefix, stamp, rest)) = split_timestamp(&it.label) {
            series.entry((prefix.to_string(), rest.to_string(), it.path.is_dir())).or_default().push((stamp.to_string(), i));
        }
    }
    for mut versions in series.into_values().filter(|v| v.len() > 1) {
        versions.sort();
        let newest = items[versions.last().unwrap().1].label.clone();
        for &(_, i) in &versions[..versions.len() - 1] {
            add_note(&mut items[i], &format!("bản cũ, đã có {newest}"));
            items[i].suggested = true;
        }
    }
}

fn add_note(it: &mut Item, note: &str) {
    it.note = Some(match it.note.take() {
        Some(prev) => format!("{prev} · {note}"),
        None => note.to_string(),
    });
}

fn item(path: PathBuf, label: String, contents_only: bool) -> Item {
    let modified = fs::symlink_metadata(&path).ok().and_then(|m| mtime(&m));
    Item { path, label, modified, contents_only, ..Default::default() }
}

fn secs(t: SystemTime) -> Option<u64> {
    t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

fn mtime(m: &fs::Metadata) -> Option<u64> {
    m.modified().ok().and_then(secs)
}

fn now_secs() -> u64 {
    secs(SystemTime::now()).unwrap_or(0)
}

fn is_real_dir(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
}

fn tilde(p: &Path, home: &Path) -> String {
    match p.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

#[repr(C)]
struct AttrList {
    bitmapcount: u16,
    reserved: u16,
    commonattr: u32,
    volattr: u32,
    dirattr: u32,
    fileattr: u32,
    forkattr: u32,
}

extern "C" {
    fn getattrlist(path: *const c_char, list: *mut AttrList, buf: *mut c_void, size: usize, options: u64) -> i32;
}

/// Bytes of a file not shared with any APFS clone, i.e. what deleting it frees.
/// `None` where the file system cannot tell (then the allocated size is the answer).
pub fn private_size(path: &Path) -> Option<u64> {
    const ATTR_CMNEXT_PRIVATESIZE: u32 = 0x8;
    const FSOPT_NOFOLLOW: u64 = 0x1;
    const FSOPT_ATTR_CMN_EXTENDED: u64 = 0x20;
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut list = AttrList {
        bitmapcount: 5,
        reserved: 0,
        commonattr: 0,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: ATTR_CMNEXT_PRIVATESIZE,
    };
    let mut buf = [0u8; 16];
    let rc = unsafe {
        getattrlist(c.as_ptr(), &mut list, buf.as_mut_ptr().cast(), buf.len(), FSOPT_NOFOLLOW | FSOPT_ATTR_CMN_EXTENDED)
    };
    let len = u32::from_ne_bytes(buf[0..4].try_into().ok()?);
    (rc == 0 && len >= 12).then(|| i64::from_ne_bytes(buf[4..12].try_into().unwrap()).max(0) as u64)
}

fn file_size(path: &Path, md: &fs::Metadata) -> u64 {
    private_size(path).unwrap_or(md.blocks() * 512)
}

/// Bytes allocated on disk under `root`, counting each hard-linked file once.
/// Returns (bytes, entries that could not be read). Never follows symlinks.
pub fn disk_usage(root: &Path) -> (u64, u64) {
    match fs::symlink_metadata(root) {
        Ok(md) if md.is_file() => return (file_size(root, &md), 0),
        Err(_) => return (0, 1),
        _ => {}
    }
    let mut seen = HashSet::new();
    let mut stack = vec![root.to_path_buf()];
    let (mut total, mut errors) = (0u64, 0u64);
    while let Some(p) = stack.pop() {
        let Ok(md) = fs::symlink_metadata(&p) else {
            errors += 1;
            continue;
        };
        if !md.is_dir() && md.nlink() > 1 && !seen.insert((md.dev(), md.ino())) {
            continue;
        }
        total += md.blocks() * 512;
        if md.is_dir() {
            match fs::read_dir(&p) {
                Ok(rd) => stack.extend(rd.flatten().map(|e| e.path())),
                Err(_) => errors += 1,
            }
        }
    }
    (total, errors)
}

struct Candidate {
    path: PathBuf,
    len: u64,
    id: (u64, u64),
    /// Creation (or modification) time: the oldest copy is taken as the original.
    born: u64,
}

#[derive(Default)]
struct Walked {
    junk: Vec<Item>,
    ai: Vec<Item>,
    dumps: Vec<Item>,
    large: Vec<Item>,
    candidates: Vec<Candidate>,
    /// Redundant copy and the copy that is kept.
    duplicates: Vec<(Item, PathBuf)>,
    unreadable: u64,
}

/// Keeps existing absolute roots and drops any that sit inside another root.
fn normalize_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut list: Vec<PathBuf> = roots.to_vec();
    list.retain(|r| r.is_absolute() && is_real_dir(r));
    list.sort();
    list.dedup();
    let mut out: Vec<PathBuf> = Vec::new();
    for r in list {
        if !out.iter().any(|o| r.starts_with(o)) {
            out.push(r);
        }
    }
    out
}

/// One pass over the roots finds project build folders, AI leftovers, dumps,
/// large files and the candidates for duplicate detection.
fn walk(roots: &[PathBuf], home: &Path, large_min: u64) -> Walked {
    let library = home.join("Library");
    let mut out = Walked::default();
    // The flag marks folders inside a git repo or project (SDKs included): files there are
    // never offered as duplicates, since tools and builds expect every copy to stay.
    let mut stack: Vec<(PathBuf, usize, bool)> = roots.iter().map(|r| (r.clone(), 0, false)).collect();
    while let Some((dir, depth, in_project)) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else {
            out.unreadable += 1;
            continue;
        };
        let entries: Vec<fs::DirEntry> = rd.flatten().collect();
        let names: HashSet<String> =
            entries.iter().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        let project = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let in_project = in_project || (depth > 0 && PROJECT_MARKERS.iter().any(|m| names.contains(*m)));
        for e in entries {
            let Ok(ft) = e.file_type() else { continue };
            let name = e.file_name().to_string_lossy().into_owned();
            let path = e.path();
            if ft.is_dir() {
                if let Some(junk) = junk_dir(&name, &names) {
                    out.junk.push(Item {
                        label: format!("{project}/{junk}"),
                        modified: project_modified(&dir, &name),
                        path,
                        ..Default::default()
                    });
                    continue;
                }
                if AI_DIRS.contains(&name.as_str()) {
                    out.ai.push(Item { label: format!("{project}/{name}"), ..item(path, String::new(), false) });
                    continue;
                }
                let ext = path.extension().and_then(|x| x.to_str()).unwrap_or("").to_lowercase();
                if name.starts_with('.') || path == library || PACKAGE_EXTS.contains(&ext.as_str()) {
                    continue;
                }
                if depth + 1 < MAX_WALK_DEPTH {
                    stack.push((path, depth + 1, in_project));
                }
            } else if ft.is_file() {
                if is_dump(&name) {
                    out.dumps.push(Item { label: format!("{project}/{name}"), ..item(path, String::new(), false) });
                    continue;
                }
                if name.starts_with('.') {
                    continue;
                }
                let Ok(md) = e.metadata() else { continue };
                if !in_project && md.len() >= DUPLICATE_MIN_BYTES {
                    let born = md.created().ok().and_then(secs).or_else(|| mtime(&md)).unwrap_or(0);
                    out.candidates.push(Candidate { path: path.clone(), len: md.len(), id: (md.dev(), md.ino()), born });
                }
                if md.blocks() * 512 >= large_min {
                    let size = file_size(&path, &md);
                    out.large.push(Item { label: name, size, modified: mtime(&md), path, ..Default::default() });
                }
            }
        }
    }
    out
}

fn junk_dir(name: &str, siblings: &HashSet<String>) -> Option<&'static str> {
    JUNK_DIRS
        .iter()
        .find(|(n, markers)| *n == name && markers.iter().any(|m| siblings.contains(*m)))
        .map(|(n, _)| *n)
}

fn is_dump(name: &str) -> bool {
    let l = name.to_lowercase();
    [".hprof", ".heapsnapshot", ".heapprofile", ".dmp", ".mdmp"].iter().any(|x| l.ends_with(x))
        || (l.starts_with("hs_err_pid") && l.ends_with(".log"))
        || ["npm-debug.log", "yarn-error.log", "pnpm-debug.log", "lerna-debug.log"].iter().any(|p| l.starts_with(p))
        || l.strip_prefix("core.").is_some_and(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_digit()))
}

/// Newest modification among a project's top-level entries, ignoring the build folder.
fn project_modified(project: &Path, junk: &str) -> Option<u64> {
    fs::read_dir(project)
        .ok()?
        .flatten()
        .filter(|e| e.file_name() != junk)
        .filter_map(|e| e.metadata().ok().and_then(|m| mtime(&m)))
        .max()
}

// ---------- duplicates ----------

/// "name (1).pdf", "name copy.pdf", "name 2.pdf": how browsers and Finder name extra copies.
fn looks_like_copy(path: &Path) -> bool {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    let digits_after = |s: &str, sep: &str| {
        s.rsplit_once(sep).is_some_and(|(_, d)| {
            let d = d.trim_end_matches(')');
            !d.is_empty() && d.len() <= 3 && d.bytes().all(|b| b.is_ascii_digit())
        })
    };
    stem.ends_with(" copy") || digits_after(&stem, " copy ") || (stem.ends_with(')') && digits_after(&stem, " (")) || digits_after(&stem, " ")
}

fn sample_hash(path: &Path, len: u64) -> Option<u64> {
    const CHUNK: u64 = 64 * 1024;
    let mut f = fs::File::open(path).ok()?;
    let mut h = DefaultHasher::new();
    let mut buf = vec![0u8; CHUNK as usize];
    let n = f.read(&mut buf).ok()?;
    h.write(&buf[..n]);
    if len > 2 * CHUNK {
        f.seek(SeekFrom::Start(len - CHUNK)).ok()?;
        let n = f.read(&mut buf).ok()?;
        h.write(&buf[..n]);
    }
    Some(h.finish())
}

fn same_contents(a: &Path, b: &Path) -> bool {
    let (Ok(mut fa), Ok(mut fb)) = (fs::File::open(a), fs::File::open(b)) else { return false };
    let (mut ba, mut bb) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    loop {
        let (Ok(na), Ok(nb)) = (read_full(&mut fa, &mut ba), read_full(&mut fb, &mut bb)) else { return false };
        if na != nb || ba[..na] != bb[..nb] {
            return false;
        }
        if na == 0 {
            return true;
        }
    }
}

fn read_full(f: &mut fs::File, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// Groups byte-identical files (same size, then sampled hash, then a full comparison) and
/// returns every redundant copy with the copy that is kept. Hard links and copies that
/// share all their blocks with a clone free nothing, so they are left out.
fn find_duplicates(candidates: Vec<Candidate>, downloads: &Path) -> Vec<(Item, PathBuf)> {
    let mut by_len: HashMap<u64, Vec<Candidate>> = HashMap::new();
    for c in candidates {
        by_len.entry(c.len).or_default().push(c);
    }
    let mut out = Vec::new();
    for (_, mut group) in by_len {
        let mut ids = HashSet::new();
        group.retain(|c| ids.insert(c.id));
        if group.len() < 2 {
            continue;
        }
        let mut by_hash: HashMap<u64, Vec<Candidate>> = HashMap::new();
        for c in group {
            if let Some(h) = sample_hash(&c.path, c.len) {
                by_hash.entry(h).or_default().push(c);
            }
        }
        for (_, same_sample) in by_hash {
            if same_sample.len() < 2 {
                continue;
            }
            let mut clusters: Vec<Vec<Candidate>> = Vec::new();
            for c in same_sample {
                match clusters.iter_mut().find(|cl| same_contents(&cl[0].path, &c.path)) {
                    Some(cl) => cl.push(c),
                    None => clusters.push(vec![c]),
                }
            }
            for mut cl in clusters.into_iter().filter(|cl| cl.len() > 1) {
                // Keep the copy outside Downloads with an original-looking name, oldest first.
                cl.sort_by_key(|c| (c.path.starts_with(downloads), looks_like_copy(&c.path), c.born, c.path.as_os_str().len()));
                let kept = cl[0].path.clone();
                for c in cl.into_iter().skip(1) {
                    let Ok(md) = fs::symlink_metadata(&c.path) else { continue };
                    let size = file_size(&c.path, &md);
                    if size == 0 {
                        continue;
                    }
                    let suggested = c.path.starts_with(downloads) || looks_like_copy(&c.path);
                    let label = c.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let mut it = Item { label, size, modified: mtime(&md), suggested, path: c.path, ..Default::default() };
                    add_note(&mut it, &format!("trùng với {}", kept.display()));
                    out.push((it, kept.clone()));
                }
            }
        }
    }
    out
}

// ---------- cleaning ----------

/// Folders that are never deleted or emptied themselves, relative to home.
const PROTECTED: &[&str] = &[
    "", "Library", "Documents", "Desktop", "Downloads", "Pictures", "Movies", "Music",
    "Applications", "Public", "Developer", ".ssh", ".config", ".claude", ".codex", ".cache",
    ".gnupg", "Library/Keychains", "Library/Application Support", "Library/Containers",
    "Library/Group Containers", "Library/Mobile Documents", "Library/Preferences",
    "Library/Developer", "Library/Caches", "Library/Logs",
];

pub fn clean(req: &CleanRequest) -> CleanResult {
    let mut roots = req.project_roots.clone();
    roots.push(req.home.join("Downloads"));
    let roots = normalize_roots(&roots);
    let mut result = CleanResult::default();
    for it in &req.items {
        if let Err(e) = check_allowed(&it.path, &req.home, &roots) {
            result.failed.push(Failure { path: it.path.clone(), error: e });
            continue;
        }
        let (before, _) = disk_usage(&it.path);
        let outcome = if it.contents_only { empty_dir(&it.path) } else { remove_path(&it.path) };
        let after = if fs::symlink_metadata(&it.path).is_ok() { disk_usage(&it.path).0 } else { 0 };
        result.freed += before.saturating_sub(after);
        match outcome {
            Ok(()) => result.removed += 1,
            Err(e) => result.failed.push(Failure { path: it.path.clone(), error: e.to_string() }),
        }
    }
    result
}

/// Only paths strictly inside home or a chosen root, not protected, and not symlinks.
pub fn check_allowed(path: &Path, home: &Path, roots: &[PathBuf]) -> Result<(), String> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
        return Err("đường dẫn không hợp lệ".into());
    }
    let inside = |base: &Path| path.starts_with(base) && path != base;
    if !inside(home) && !roots.iter().any(|r| inside(r)) {
        return Err("nằm ngoài thư mục được phép".into());
    }
    if PROTECTED.iter().any(|p| path == home.join(p)) || roots.iter().any(|r| path == r) {
        return Err("thư mục được bảo vệ".into());
    }
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => Err("là symlink".into()),
        Ok(_) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

fn remove_path(p: &Path) -> io::Result<()> {
    let attempt = |p: &Path| {
        let md = fs::symlink_metadata(p)?;
        if md.is_dir() { fs::remove_dir_all(p) } else { fs::remove_file(p) }
    };
    match attempt(p) {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            // Go's module cache and similar trees are read-only on purpose.
            make_dirs_writable(p);
            attempt(p)
        }
        other => other,
    }
}

fn empty_dir(dir: &Path) -> io::Result<()> {
    let mut first_err = None;
    for e in fs::read_dir(dir)?.flatten() {
        if let Err(err) = remove_path(&e.path()) {
            first_err.get_or_insert(err);
        }
    }
    first_err.map_or(Ok(()), Err)
}

fn make_dirs_writable(root: &Path) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(md) = fs::symlink_metadata(&p) else { continue };
        if !md.is_dir() {
            continue;
        }
        let mut perms = md.permissions();
        perms.set_mode(perms.mode() | 0o700);
        let _ = fs::set_permissions(&p, perms);
        if let Ok(rd) = fs::read_dir(&p) {
            stack.extend(rd.flatten().map(|e| e.path()));
        }
    }
}

// ---------- C interface ----------

fn json_call<I: for<'de> Deserialize<'de>, O: Serialize>(input: *const c_char, f: impl FnOnce(I) -> O) -> *mut c_char {
    let out = (|| -> Result<String, String> {
        if input.is_null() {
            return Err("null input".into());
        }
        let text = unsafe { CStr::from_ptr(input) }.to_str().map_err(|e| e.to_string())?;
        let parsed: I = serde_json::from_str(text).map_err(|e| e.to_string())?;
        serde_json::to_string(&f(parsed)).map_err(|e| e.to_string())
    })()
    .unwrap_or_else(|e| serde_json::json!({ "error": e }).to_string());
    CString::new(out).map_or(std::ptr::null_mut(), CString::into_raw)
}

/// `config_json` is a ScanConfig; returns a ScanResult (or `{"error": …}`). Free with `dustpan_free`.
#[no_mangle]
pub extern "C" fn dustpan_scan(config_json: *const c_char) -> *mut c_char {
    json_call(config_json, |cfg: ScanConfig| scan(&cfg))
}

/// `request_json` is a CleanRequest; returns a CleanResult (or `{"error": …}`). Free with `dustpan_free`.
#[no_mangle]
pub extern "C" fn dustpan_clean(request_json: *const c_char) -> *mut c_char {
    json_call(request_json, |req: CleanRequest| clean(&req))
}

/// # Safety
/// `s` must come from `dustpan_scan` or `dustpan_clean` and be freed once.
#[no_mangle]
pub unsafe extern "C" fn dustpan_free(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempHome(PathBuf);
    impl TempHome {
        fn new(name: &str) -> Self {
            let p = std::env::temp_dir().join(format!("dustpan-test-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            TempHome(p)
        }
        fn file(&self, rel: &str, bytes: usize) -> PathBuf {
            self.file_with(rel, &vec![7u8; bytes])
        }
        fn file_with(&self, rel: &str, data: &[u8]) -> PathBuf {
            let p = self.0.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, data).unwrap();
            p
        }
    }
    impl Drop for TempHome {
        fn drop(&mut self) {
            make_dirs_writable(&self.0);
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn cat<'a>(r: &'a ScanResult, id: &str) -> &'a Category {
        r.categories.iter().find(|c| c.id == id).unwrap()
    }

    fn labels(r: &ScanResult, id: &str) -> Vec<String> {
        let mut v: Vec<String> = cat(r, id).items.iter().map(|i| i.label.clone()).collect();
        v.sort();
        v
    }

    fn bytes(n: usize, seed: u8) -> Vec<u8> {
        (0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
    }

    #[test]
    fn scan_finds_caches_junk_and_large_files() {
        let h = TempHome::new("scan");
        h.file("Library/Caches/com.example.app/blob", 10_000);
        h.file("Library/Caches/Homebrew/pkg.tar.gz", 10_000);
        h.file("code/web/package.json", 10);
        h.file("code/web/node_modules/left-pad/index.js", 20_000);
        h.file("code/rs/Cargo.toml", 10);
        h.file("code/rs/target/debug/app", 20_000);
        h.file("code/notes/target/keep.txt", 20_000); // no Cargo.toml: not junk
        h.file_with("Movies/big.mov", &bytes(3 * 1024 * 1024, 1));

        let r = scan(&ScanConfig { home: h.0.clone(), project_roots: vec![], large_file_mb: 2 });

        assert_eq!(labels(&r, "app_caches"), ["com.example.app"], "Homebrew belongs to package caches only");
        assert!(cat(&r, "app_caches").items[0].suggested);
        assert_eq!(labels(&r, "package_caches"), ["Homebrew downloads"]);
        assert_eq!(labels(&r, "project_junk"), ["rs/target", "web/node_modules"]);
        assert_eq!(labels(&r, "large_files"), ["big.mov"]);
        assert!(!cat(&r, "large_files").items[0].suggested);
    }

    #[test]
    fn scan_finds_ai_leftovers_and_dumps() {
        let h = TempHome::new("ai");
        h.file("Library/Application Support/Claude/Cache/data_0", 5000);
        h.file("Library/Application Support/Claude/config.json", 5000); // real data: not listed
        h.file("Library/Application Support/Code/CachedExtensionVSIXs/x.vsix", 5000);
        h.file("Library/Application Support/Code/Crashpad/completed/a.dmp", 5000);
        h.file(".claude/projects/-Users-me-app/s1.jsonl", 5000);
        h.file(".claude/shell-snapshots/snap.sh", 5000);
        h.file(".cache/huggingface/hub/models--bert/blob", 5000);
        h.file(".cache/node/corepack.tgz", 5000);
        h.file("code/site/.playwright-mcp/page.png", 5000);
        h.file("code/api/java_pid42.hprof", 5000);
        h.file("code/api/.core.dump", 10);
        h.file("code/api/core.1234", 5000);
        h.file("code/api/core.c", 5000); // source file, not a dump

        let r = scan(&ScanConfig { home: h.0.clone(), project_roots: vec![], large_file_mb: 1024 });

        assert_eq!(
            labels(&r, "ai_caches"),
            ["Claude Code · shell snapshots", "Claude · Cache", "Code · CachedExtensionVSIXs"]
        );
        assert_eq!(labels(&r, "ai_history"), ["-Users-me-app"]);
        assert!(!cat(&r, "ai_history").items[0].suggested, "fresh sessions stay unselected");
        assert_eq!(labels(&r, "ai_models"), ["models--bert"]);
        assert_eq!(labels(&r, "hidden_caches"), ["node"], "huggingface is a model store, not a cache");
        assert_eq!(labels(&r, "ai_project_files"), ["site/.playwright-mcp"]);
        assert_eq!(labels(&r, "dumps"), ["Code · Crashpad/completed", "api/core.1234", "api/java_pid42.hprof"]);
    }

    #[test]
    fn downloads_and_duplicates() {
        let h = TempHome::new("dl");
        let video = bytes(2 * 1024 * 1024, 9);
        h.file_with("Movies/talk.mp4", &video);
        h.file_with("Downloads/talk.mp4", &video);
        h.file_with("Documents/a.pdf", &bytes(1024 * 1024 + 5, 3));
        h.file_with("Documents/a copy.pdf", &bytes(1024 * 1024 + 5, 3));
        let mut almost = bytes(1024 * 1024 + 5, 3);
        almost[500_000] ^= 1; // same size and sampled bytes, different content
        h.file_with("Documents/b.pdf", &almost);
        h.file("Downloads/Tool.dmg", 4000);
        h.file("Downloads/movie.mkv.crdownload", 4000);
        h.file("Downloads/site.tar.gz", 4000);
        h.file("Downloads/site/index.html", 4000);
        h.file("Downloads/notes.txt", 4000);
        h.file("sdk/flutter/.git/HEAD", 10);
        h.file_with("sdk/flutter/bin/cache/a.dill", &bytes(1024 * 1024 + 7, 4));
        h.file_with("sdk/flutter/bin/cache/b.dill", &bytes(1024 * 1024 + 7, 4));

        let r = scan(&ScanConfig { home: h.0.clone(), project_roots: vec![], large_file_mb: 1024 });

        let dl = &cat(&r, "downloads").items;
        let get = |name: &str| dl.iter().find(|i| i.label == name).unwrap();
        assert!(get("Tool.dmg").suggested);
        assert!(get("movie.mkv.crdownload").suggested);
        assert!(get("site.tar.gz").note.as_deref().unwrap().contains("site/"));
        assert!(get("site.tar.gz").suggested && !get("site").suggested);
        assert!(!get("notes.txt").suggested && get("notes.txt").note.is_none());
        let talk = get("talk.mp4");
        assert!(talk.suggested && talk.note.as_deref().unwrap().contains("Movies/talk.mp4"), "{talk:?}");

        // The Downloads copy is listed under Downloads only; "a copy.pdf" is the extra one.
        let dups = &cat(&r, "duplicates").items;
        assert_eq!(dups.len(), 1, "{dups:?}");
        assert_eq!(dups[0].label, "a copy.pdf");
        assert!(dups[0].suggested);
    }

    #[test]
    fn dated_series_keep_newest() {
        assert_eq!(
            split_timestamp("theciu_2026-10-19T07-35-04-682Z.tar.gz"),
            Some(("theciu_", "2026-10-19T07-35-04-682Z", ".tar.gz"))
        );
        assert_eq!(split_timestamp("db_2026-03-17_03-46-03.dump"), Some(("db_", "2026-03-17_03-46-03", ".dump")));
        assert_eq!(split_timestamp("2026-03-17 notes.txt"), None, "a date alone is not a series");
        assert_eq!(split_timestamp("Phieu 20042026.xlsx"), None);

        let h = TempHome::new("series");
        h.file("Downloads/shop_2026-10-01T01-00-00-000Z/db.sql", 4000);
        h.file("Downloads/shop_2026-11-01T01-00-00-000Z/db.sql", 4000);
        h.file("Downloads/shop_2026-11-01T01-00-00-000Z.tar.gz", 4000);
        h.file("Downloads/db_2026-03-17_03-46-03.dump", 4000);
        h.file("Downloads/db_2026-04-02_18-18-44.dump", 4000);
        let r = scan(&ScanConfig { home: h.0.clone(), project_roots: vec![], large_file_mb: 1024 });
        let mut picked: Vec<String> =
            cat(&r, "downloads").items.iter().filter(|i| i.suggested).map(|i| i.label.clone()).collect();
        picked.sort();
        // The newest archive goes too, since its extracted folder sits next to it.
        assert_eq!(
            picked,
            ["db_2026-03-17_03-46-03.dump", "shop_2026-10-01T01-00-00-000Z", "shop_2026-11-01T01-00-00-000Z.tar.gz"]
        );
    }

    #[test]
    fn copy_names() {
        for yes in ["x (1).pdf", "x copy.pdf", "x copy 2.pdf", "IMG 2.jpg"] {
            assert!(looks_like_copy(Path::new(yes)), "{yes}");
        }
        for no in ["x.pdf", "report-2.pdf", "AL6061 T6.pdf", "v (beta).zip"] {
            assert!(!looks_like_copy(Path::new(no)), "{no}");
        }
    }

    #[test]
    fn clones_free_nothing() {
        let h = TempHome::new("clone");
        let a = h.file_with("a.bin", &bytes(2 * 1024 * 1024, 5));
        let b = h.0.join("b.bin");
        let ok = std::process::Command::new("cp").arg("-c").arg(&a).arg(&b).status().unwrap().success();
        assert!(ok);
        assert!(private_size(&a).unwrap() < 1024 * 1024, "APFS clone shares its blocks");
        assert_eq!(private_size(&h.file_with("c.bin", &bytes(1024 * 1024, 6))), Some(1024 * 1024));
    }

    #[test]
    fn clean_removes_items_and_keeps_contents_only_folders() {
        let h = TempHome::new("clean");
        h.file("Library/Caches/a/x", 5000);
        h.file(".Trash/old.zip", 5000);
        let ro = h.file("go/pkg/mod/m@v1/f.go", 5000);
        let ro_dir = ro.parent().unwrap();
        fs::set_permissions(ro_dir, fs::Permissions::from_mode(0o555)).unwrap();

        let r = clean(&CleanRequest {
            home: h.0.clone(),
            project_roots: vec![],
            items: vec![
                CleanItem { path: h.0.join("Library/Caches/a"), contents_only: false },
                CleanItem { path: h.0.join(".Trash"), contents_only: true },
                CleanItem { path: h.0.join("go/pkg/mod"), contents_only: true },
            ],
        });
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        assert_eq!(r.removed, 3);
        assert!(r.freed >= 15_000);
        assert!(!h.0.join("Library/Caches/a").exists());
        assert!(h.0.join(".Trash").is_dir());
        assert_eq!(fs::read_dir(h.0.join(".Trash")).unwrap().count(), 0);
        assert_eq!(fs::read_dir(h.0.join("go/pkg/mod")).unwrap().count(), 0);
    }

    #[test]
    fn clean_refuses_unsafe_paths() {
        let h = TempHome::new("refuse");
        let outside = TempHome::new("outside");
        let precious = outside.file("precious.txt", 100);
        h.file("Documents/a.txt", 100);
        std::os::unix::fs::symlink(&outside.0, h.0.join("link")).unwrap();

        let bad = [
            h.0.clone(),
            h.0.join("Documents"),
            h.0.join("Library"),
            h.0.join(".claude"),
            precious.clone(),
            h.0.join("link"),
            h.0.join("Documents/../Documents"),
        ];
        let r = clean(&CleanRequest {
            home: h.0.clone(),
            project_roots: vec![],
            items: bad.iter().map(|p| CleanItem { path: p.clone(), contents_only: false }).collect(),
        });
        assert_eq!(r.removed, 0);
        assert_eq!(r.failed.len(), bad.len());
        assert!(precious.exists());
        assert!(h.0.join("Documents/a.txt").exists());
    }

    #[test]
    fn hard_links_count_once() {
        let h = TempHome::new("links");
        let a = h.file("d/a", 50_000);
        fs::hard_link(&a, h.0.join("d/b")).unwrap();
        let (with_link, _) = disk_usage(&h.0.join("d"));
        fs::remove_file(h.0.join("d/b")).unwrap();
        let (without, _) = disk_usage(&h.0.join("d"));
        assert_eq!(with_link, without);
    }

    #[test]
    fn ffi_round_trip_and_errors() {
        let input = CString::new("not json").unwrap();
        let out = dustpan_scan(input.as_ptr());
        let text = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_owned();
        unsafe { dustpan_free(out) };
        assert!(text.contains("\"error\""));
    }
}
