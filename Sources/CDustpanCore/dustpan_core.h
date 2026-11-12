#pragma once

// Every function takes a UTF-8 JSON string and returns a newly allocated JSON string
// (or {"error": "..."}) that must be released with dustpan_free.

/// ScanConfig {home, project_roots, large_file_mb} -> ScanResult.
char *dustpan_scan(const char *config_json);
/// CleanRequest {home, project_roots, items: [{path, contents_only}]} -> CleanResult.
char *dustpan_clean(const char *request_json);
void dustpan_free(char *s);
