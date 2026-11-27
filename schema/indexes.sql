-- eg schema v12 secondary indexes.
-- A fresh build creates these after the bulk insert, so each index is sorted
-- once instead of maintained per row.
-- Covering reverse-edge index: BFS reads src/site/confidence without table lookups.
CREATE INDEX edges_dst_cover ON edges(dst_id, kind, src_id, site_line, confidence);
CREATE INDEX edges_src ON edges(src_id, kind, dst_id, site_line, confidence);
-- Test-symbol anchor (v8): partial index covers only the test set.
CREATE INDEX symbols_is_test ON symbols(is_test, kind, name) WHERE is_test = 1;
CREATE INDEX symbols_file ON symbols(file_id);
-- Path-scope anchor: `--path` range scans stay on file_path.
CREATE INDEX symbols_file_path ON symbols(file_path, line_start, qualname);
CREATE INDEX symbols_qualname ON symbols(qualname);
CREATE INDEX unresolved_calls_file_kind_line ON unresolved_calls(file_id, kind, site_line, dst_name);
CREATE INDEX unresolved_calls_kind ON unresolved_calls(kind, dst_name);
CREATE INDEX symbols_name ON symbols(name);
CREATE INDEX class_fields_file ON class_fields(file_id);
CREATE INDEX class_fields_class ON class_fields(class_qualname, field);
CREATE INDEX module_exports_file ON module_exports(file_id);
CREATE INDEX module_stars_file ON module_stars(file_id);
