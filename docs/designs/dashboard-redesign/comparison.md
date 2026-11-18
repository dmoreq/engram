# So sánh 5 phương án thiết kế lại Dashboard (`eg dashboard`)

- Ngày: 2026-11-17
- Phạm vi: tổng hợp 5 spec mockup đặt cùng thư mục với báo cáo này (`option-1..5-*/mockup.html`) — bài toán thiết kế lại dashboard từ số 0, không dựa trên dashboard hiện tại — cộng với kiểm chứng file và nguồn chạy trực tiếp trong phiên này (Phụ lục A).
- Mục đích: tài liệu ra quyết định cho chủ sở hữu. Báo cáo không xếp hạng và không chọn giúp; phần 4 chỉ ánh xạ tình huống sử dụng sang phương án phù hợp.

**Minimalism revision v2 — 2026-11-17.** Cả 5 mockup đã qua một lượt minimalism theo doctrine chặt: design-system Static Minimalism Gate + ux-product-shaper 8-state + ui-accessibility WCAG 2.2 AA. Mỗi option đi đủ 2 vòng (design → peer review → fix → verify cycle 2 độc lập); cả 5 Pass. Dòng code giảm ở 4/5: O1 871→769 (−12%), O2 908→835 (−8%), O3 607→607 (0% — mốc giữ nguyên sau khi nén để chứa a11y fixes), O4 908→841 (−7%), O5 1010→1004 (−1%). Shared design system hội tụ có chủ đích: 1 bộ 25 semantic tokens Shibumi (nguồn `src/dashboard/assets/style.css:1-60`), chip vocabulary thống nhất 1 class `.chip` + intent variants, type ramp ≤5 cỡ (O2: ≤9 editorial roles), text floor 10px (muted ≥11px), theme key chung `eg-theme`, `prefers-reduced-motion` đầy đủ cả 5 (CSS + JS), `focus-visible` cả 2 theme, control boundary ≥3:1, reflow breakpoint ≤900px. Hạt nhân concept/IA từng option không đổi — mục 2 giữ phân tích gốc và thêm khối Surface Mode + Minimalism v2 ở đầu mỗi mục; watch-items và caveats (mục 5) đã cập nhật theo vòng verify v2.

## TL;DR

5 phương án phân hóa theo 3 trục loại trừ lẫn nhau: mô hình IA, primary job, ngôn ngữ thị giác. Cả 5 cùng giữ ràng buộc repo (token Shibumi light/dark, vanilla tự chứa 100%), nên sự khác biệt thật nằm ở typography, mật độ và ngữ pháp bố cục. Kiểm chứng độc lập sau bản minimalism v2 xác nhận 10/10 cặp khác nhau đủ 3/3 trục (IA / visual+density / primary job) với signature còn nguyên; cặp gần nhau nhất giờ là 2↔5 (watch-item — mục 5). Chọn theo primary job: giám sát realtime (1), catch-up đọc lại (2), điều tra không gian mã nguồn (3), phiên keyboard một-luồng-việc (4), audit theo dòng thời gian (5).

## 1. Bối cảnh và ràng buộc chung

engram (`eg`) là binary Rust chạy hoàn toàn cục bộ với 3 trụ cột: đồ thị mã xác định (code graph, truy vấn <2ms), engine chống tái phát minh (anti-reinvention), và bộ nhớ dự án bền vững không tốn token — phục vụ cả developer lẫn AI coding agent qua CLI và MCP. `eg dashboard` là console vận hành khởi động bằng `eg dashboard`: offline trên loopback 127.0.0.1, đọc `usage.log`, `index.db`, `memory.db` trong `.engram/`, stream SSE thời gian thực, toàn bộ asset nhúng vào binary. Người dùng duy nhất là developer giám sát/điều tra hành vi của nhiều agent trên nhiều repo và worktree — không auth, không đa người dùng.

Đây là 5 phương án thiết kế lại dashboard từ số 0. Cả 5 cùng giữ các ràng buộc kỹ thuật của repo:

- HTML/CSS/JS vanilla tự chứa 100% trong một file: không CDN, không web font ngoài, không build step.
- Light + dark: token Shibumi của repo (nguồn `src/dashboard/assets/style.css:1-60`); `data-theme` kết hợp `prefers-color-scheme`. Bản v2 chuẩn hóa cả 5 về đúng 1 bộ 25 semantic tokens và key theme chung `eg-theme`.
- Không emoji, không ký tự sparkline/bar Unicode; thanh tỉ lệ là div CSS thuần.
- Không bịa endpoint hay trường không có trong payload thật; số liệu demo được kê khai tường minh trong từng file.

Cách mở: mở `mockup.html` trong từng thư mục bằng trình duyệt (file tĩnh, không cần server). Đường dẫn dưới đây tính từ thư mục `docs/designs/dashboard-redesign/` (nơi chứa báo cáo này):

| # | Phương án | Đường dẫn mockup | Dòng v2 (wc -l) |
|---|---|---|---|
| 1 | Mission Control | `option-1-mission-control/mockup.html` | 769 |
| 2 | Editorial Journal | `option-2-editorial-journal/mockup.html` | 835 |
| 3 | Spatial Graph Canvas | `option-3-graph-canvas/mockup.html` | 607 |
| 4 | Command Workbench | `option-4-command-workbench/mockup.html` | 841 |
| 5 | Chronicle Timeline | `option-5-chronicle-timeline/mockup.html` | 1004 |

## 2. Phân tích từng phương án

Mỗi mục giữ nguyên phân tích hạt nhân (ý tưởng, IA, ngôn ngữ thị giác, data binding, best-fit — không đổi qua v1 → v2); khối **Surface Mode** + **Minimalism v2** đầu mỗi mục kê khai trạng thái hiện hành: số dòng, cuts chính, bổ sung, trạng thái verify. Chi tiết trong phần thân (số ô KPI, briefing, lane, minimap...) phản ánh thiết kế v1 trừ khi khối v2 nói khác.

### 2.1 Option 1 — Mission Control

**File**: `option-1-mission-control/mockup.html` (769 dòng; v1 871 → 769, −12%).

**Surface Mode**: Mode B — Shibumi Utilitarian (operate & read); file tự khai "Mode B density scale" (`mockup.html:42`).

**Minimalism v2** (871 → 769 dòng, −12%; verify Pass ở verify cycle 2 độc lập): KPI strip 9 → 4 tín hiệu (các chỉ số phụ về đúng panel chủ quản); R3 TOOL LATENCY gộp vào feed; evidence strip gộp vào inspector — còn đúng 1 drill-down duy nhất; bỏ `--intent-memory` violet, memory dùng `--intent-info`; thêm Loading state (phase SSE handshake, tách khỏi PAUSED) theo 8-state; focus-visible cả 2 theme, control boundary ≥3:1, reflow ≤900px theo shared system (mục 3).

**Ý tưởng một câu**: Bàn điều khiển operator một màn hình cố định chiều cao — KPI strip 9 ô phía trên, live agent-activity feed làm tâm, 6 bảng tín hiệu phụ xếp quanh, evidence strip full-width cho chứng cứ song song; không tab, không drill-down làm chế độ chính.

**IA**: Một viewport 1440×900 gồm 4 band, console 100vh `overflow:hidden`:

1. Topbar: brand + window chip + DATA AGE + nút FIRST-RUN + LIVE/PAUSED + theme toggle.
2. KPI strip 9 ô: TOTAL CALLS, LATENCY, TOKENS SAVED, GRAPH QUERIES, ERRORS (error_count + reliability per pillar), SESSIONS, PRE-WRITE LOOKUPS, DUPLICATE POOL, RESOLVED TWINS.
3. Grid 3 cột: rail trái (L1 REPO SIGNALS — 8 cột gồm MEM, sub-row worktree đúng WtAgg; L2 AGENT ATTRIBUTION — 7 cột có MEM; L3 PRE-WRITE DISCOVERY) | cột giữa LIVE AGENT ACTIVITY — feed 8 cột với filter status/repo/worktree/agent/search (options sinh từ dữ liệu render), inspector mở rộng được | rail phải (R1 DUPLICATES, R2 MEMORY INTEGRITY, R3 TOOL LATENCY).
4. Evidence strip full-width: snippet CANDIDATE/CANONICAL song song (~690px mỗi cột, scroll ngang); mở bằng click/Enter trên cand-row, đóng bằng nút hoặc ESC. Statusbar: endpoint + schema_version + SSE state + "window 50 entries" + generated_at + version.

**Ngôn ngữ thị giác**: All-mono terminal chuẩn — font mono hệ thống, tabular-nums, thang 8.5–17px, nhãn uppercase letter-spacing. Token Shibumi (v2 bỏ `--intent-memory` violet; memory dùng `--intent-info`); ánh xạ hue: info=graph, warning=reuse, memory=info, danger=errors. `color-scheme` khai báo từng khối token để form control theo đúng theme. Breakpoint 1101–1250px co rail xuống 280/292px, <1100px fallback stack. Keyboard-first: role=button + tabindex + aria-pressed trên mọi row tương tác, ArrowUp/Down duyệt feed, Enter/Space chọn, `/` focus search, ESC đóng evidence.

**Key components**:

- FIRST-RUN scenario (nút trong topbar): KPI ra dash + "no samples yet", feed/panels hiện empty-state hint, statusbar "0 journal entries", DATA AGE —. Contract null chung: dash thay số, không bịa 0 (xác nhận trong phiên: hàm `setFirstRun` tại `mockup.html:830-856`).
- KPI ERRORS (9, rel 99.3% · graph 99.6%) thay ô JOURNAL WINDOW đã bỏ; PRE-WRITE LOOKUPS ghi rõ nguồn trường và "L1 REU sums 103" để tách bạch với reuse_calls.
- L1 REPO SIGNALS: 4 repo + cột MEM từ memory_calls; sub-row worktree chỉ calls/err/last_ts (đúng WtAgg, các cột còn lại là dash).
- Feed inspector: projection đủ trường journal entry thật (ts, tool, repo, repo_name, agent, exit_code, shown, result_count, latency_us, args + repo_main/repo_main_name khi worktree).
- R1 DUPLICATES: dòng "pool 38" (total_candidates) tách khỏi "sample · cap 25/repo · est" (detector_counts đếm trên sample) + tag limit-capped cho docs-site.
- R2 MEMORY INTEGRITY: VERIFY/VERIFY ALL chỉ re-anchor — sau POST 200, chip giữ nguyên trạng thái modified/broken, meta đổi thành "anchored just now".
- R3 TOOL LATENCY: sort theo calls giảm dần, qualifier "top 8 of 10 · 1,126/1,284 calls".

**Data binding**: Mọi widget gắn trường thật đã xác minh trong nguồn: /api/summary (repo_aggs serialize tại `src/dashboard/data/summary/payload.rs:242-247` — phiên này đã Read xác nhận các trường memory_calls/memory_reads/memory_writes/memory_read_hit_rate_pct; worktrees WtAgg {calls, errors, last_ts} theo spec tại `src/dashboard/data/summary.rs:365-369`), /api/reuse (detector_counts đếm trên sample + cờ `detector_counts_estimated` tại `src/dashboard/data/reuse.rs:464-472, 490-492` — đã Read xác nhận), /api/memories, POST /api/memory/touch (200 chỉ re-anchor), SSE /events cap 50. Row demo ngoài samplePayloads được thiết kế khớp tổng KPI (1284 calls / 9 err / 412k tok / 906 graph / 46 memory_calls — assert bằng script của spec).

**Trade-offs**:

- Độ sâu điều tra bị hy sinh: không dossier đầy đủ, chứng cứ gọn trong evidence strip 2 cột; xem code nguồn thật vẫn cần CLI/editor.
- Feed giữ giới hạn backend thật: 50 entry mới nhất, filter chỉ hiệu lực trong cửa sổ đó — ghi rõ trên UI ("cap 50 · filter in-window") thay vì giả time-range/pagination.
- Row demo ngoài samplePayloads là filler bịa nhưng khớp tổng KPI và khớp qualifier.
- File v2 769 dòng (v1 871), vẫn trên dải 300–600 của brief ban đầu; minimalism v2 cắt 12% mà không bỏ fix major nào.
- Chưa render headless (máy chỉ có Safari) — nhận xét layout dựa trên grid math; empty-state chỉ làm đại diện toàn cục (FIRST-RUN).

**Best-fit**: Giám sát realtime nhiều agent trên nhiều repo/worktree: operator mở "eg dashboard" thường trực, nhìn thấy ngay lời gọi lỗi, ref memory stale, candidate trùng lặp (kèm chứng cứ ở evidence strip), giá trị token tiết kiệm; duyệt event bằng bàn phím không cần chuột.

**Khác biệt cốt lõi so với 4 phương án còn lại**: Duy nhất theo mô hình "mọi thứ hiện cùng lúc" — console tín hiệu cố định 4 band không scroll trang, không palette, không timeline, không canvas. Ngữ pháp thị giác all-mono terminal mật độ cao tách khỏi Georgia editorial (2) và node-map đứt nét (3). Job ambient watch được giữ riêng cho phương án này, và FIRST-RUN là contract trạng thái null duy nhất trong 5 phương án.

### 2.2 Option 2 — Editorial Journal

**File**: `option-2-editorial-journal/mockup.html` (835 dòng; v1 908 → 835, −8%).

**Surface Mode**: Mode A — Swiss / Zen Editorial (persuade & narrate): nhịp đọc tài liệu, display hero, space âm có chủ đích.

**Minimalism v2** (908 → 835 dòng, −8%; verify Pass cycle 2): display ramp gọn về 1 display hero 44px duy nhất (`--fs-figure`); lead ràng buộc ≤25 từ; detector pills 8 → 3 (All/D1/D1L — D1L là empty-state thật khi lọc); est marker chuyển sang text + title (không color-alone); thêm Loading state cho touch in-flight; fix Critical runtime: wire filter bị `.entry{display:grid}` ghi đè `[hidden]` (đã sửa). Trần type riêng của option: ≤9 editorial roles (các option khác ≤5 cỡ).

**Ý tưởng một câu**: Dashboard đọc như một số báo định kỳ — masthead, folio running head, lead tự sự, pull-quote bất đối xứng, margin annotations — nhưng mọi khung tự sự tuyên bố phạm vi trung thực "retained journal window", không hứa delta since-last-open.

**IA**: Cuộn dọc theo desks: Folio sticky running head theo dõi section đang đọc qua IntersectionObserver → Masthead (chip Live SSE, Data-age = floor(now−generated_at), theme) → The Lead: headline "What the retained journal recorded", câu phạm vi nằm ngay trong đoạn lead (không có khung "since you last opened" vì eg không có baseline last-visit) → I. Graph Desk (scope graph_* thuần, Dispatch Roster từ agent_aggs) → II. Duplication Bureau (pills floor/pool tách lớp; twin dossier hoán đổi được qua click floor-preview; Pre-write dispatches từ prewrite_searches) → III. Memory Desk (anchor ledger render từ notes[]; nhánh rỗng renders "All anchors held") → IV. Live Wire (tool pills sinh từ entries; select repo/agent; drill-in mở args + result_count) → Appendix A (ledger app + sub-row worktree task-42 + ingest + parser) → Colophon kê khai đầy đủ demo-filler và semantics.

**Ngôn ngữ thị giác**: Token Shibumi giữ nguyên (ràng buộc repo); khác biệt hóa bằng ngôn ngữ báo chí: Georgia display 84/56/42px, cột chữ 62ch, grid gutter 216px, pull-quote bất đối xứng (.pq-right max 560px lệch phải), mono 12.5px. Hệ thống đánh dấu 2 lớp số liệu: est chấm-dotted cảnh báo cho mọi số sau cờ *_estimated (412,000 / 305,000 / 1,240 / 6.4), số solid cho floor, nhãn "· pool" riêng cho All pill. Hover/focus/active đầy đủ; skip-link.

**Key components**:

- Folio running head sticky cập nhật section qua IntersectionObserver.
- The Lead với câu phạm vi in-line + pull-quote 412,000 est (caption khai báo graph 305,000 est và từ chối suy diễn phần còn lại).
- Dispatch Roster mới từ agent_aggs (calls/errors/graph_calls/saved_tokens).
- Twin dossier hoán đổi được: click floor-preview row → featured twin thay snippet (candidates[] có candidate_snippet/canonical_snippet per candidate) + overlap bar cập nhật động.
- Anchor ledger render từ notes[] (4 note demo: 1 modified, 1 broken, 1 valid, 1 unverified+superseded); Re-anchor chỉ in response thật và giữ nguyên chip modified; thêm chip demo 409 `{ok:false,error}` (xác nhận trong phiên: `mockup.html:878-891`).
- Live Wire: tool pills sinh từ entries (production: tool_aggs keys), select repo/agent, entry mem_query (tên journaled thật) + entry worktree có repo_main trong drill-in.
- Colophon: 5 endpoint + legend est/floor/pool + nguồn thời gian data-age + kê khai demo-filler theo section.

**Data binding**: /api/summary (saved_tokens_estimated `payload.rs:151`, graph_saved_tokens_estimated `payload.rs:157`, entry shape `payload.rs:314` — theo spec), /api/reuse (total_candidates 38 pool; preventable_loc 1,240 estimated; avg_callers 6.4 + `avg_callers_estimated` tại `reuse.rs:489`; detector_counts in như SAMPLE FLOOR tại `reuse.rs:471-489` — phiên này Read xác nhận comment nguồn "Counts come from the sampled candidate list, not the whole corpus"), /api/memories notes[] (render từ mảng, không hard-code), POST touch 200/409 đúng shape, /events gồm tool mem_query (`from_cli.rs:228-251` theo spec).

**Trade-offs**:

- 835 dòng (v1 908, từ 606 vòng trước) sau khi minimalism v2 cắt 8%; các fix major bắt buộc vẫn giữ khối render từ dữ liệu; vẫn tự chứa 100%.
- Khung "since you last open" bị loại hoàn toàn thay vì tính delta từ localStorage — trung thực hơn nhưng mất kịch tính "catch-up"; có thể quay lại nếu sau này thêm baseline last-visit.
- Không render headless (máy chỉ có Safari) — dùng short-label + title đầy đủ thay vì tuyên bố wrap hoàn hảo; nhận định layout dựa trên CSS/grid math.
- Lọc worktree trên wire chưa wire vào mockup (chỉ có repo/agent select); demo kê khai tường minh theo section; 4 arrivals mô phỏng là JS interval, không phải SSE thật.
- Drill-in của wire dùng aria-expanded trên li thay vì dialog riêng — yếu hơn inspector panel cho debug sâu.

**Best-fit**: Developer quay lại sau vài giờ/vài ngày cần đọc lại bình tĩnh và ra quyết định: review từng twin bằng chứng cứ snippet hoán đổi được ngay trong trang, thấy ngay anchor nào drifted, biết agent nào đang gây lỗi qua roster, lọc dispatch theo repo/agent và mở rộng args khi điều tra — với mỗi con số đều tự khai báo phạm vi (retained window), lớp đo (floor vs pool) và độ tin cậy (est).

**Khác biệt cốt lõi so với 4 phương án còn lại**: Duy nhất theo mô hình "tài liệu cuộn dọc theo desks" với folio running head và typography Georgia editorial pull-quote — các phương án còn lại là console, canvas, palette hoặc stream. Job catch-up + ra quyết định được giữ riêng; các phương án khác chủ ý de-scope job này. Việc loại khung delta since-last-open là lựa chọn trung thực vì dữ liệu không có baseline — đặc tính không phương án nào tuyên bố.

### 2.3 Option 3 — Spatial Graph Canvas

**File**: `option-3-graph-canvas/mockup.html` (607 dòng — ngắn nhất trong 5; giữ nguyên mốc qua v1 → v2).

**Surface Mode**: Mode C — Shoji / Studio Hybrid (create & inspect): canvas làm sân khấu chính, context rail hai bên.

**Minimalism v2** (607 → 607 dòng, 0% — mốc giữ nguyên: nén cấu trúc để chứa a11y fixes; verify Pass cycle 2): cắt minimap (tương tác drag-only, không có đường keyboard tương đương); ticker tape gộp về tab Live — Live là bề mặt journal duy nhất; Impact tab bám selection thật; memory nav mở đúng note được click; palette jump center node; thêm keyboard pan/zoom + LOD + hit target 24px screen-space; media query ≤900px.

**Ý tưởng một câu**: Đồ thị mã nguồn là giao diện — canvas node-map repo → module → symbol chiếm trung tâm, khởi động bằng fit-to-content thật (không cắt badge), có minimap kéo-pan và ping/flash khi journal trúng node; mọi điều tra journal→node→dossier chạy hub-and-spoke không scroll trang.

**IA**: Grid 3 cột 276|1fr|344 + topbar + journal bar; body `overflow:hidden`.

- Topbar: 6 chip KPI từ /api/summary, gồm chip Data age đếm từ generated_at và chip Live 3 trạng thái (idle xám mặc định / streaming / paused gắn diễn giải EventSource readyState).
- Dock trái = scope rail collapsible (details/summary): Scope (gồm errors + reliability_pct + graph_reliability_pct), Agents (agent_aggs với window-count là sub-label), Anti-reinvention (detector pills), Memory health (danh sách đa note + headline đếm + empty-state "all anchors held").
- Canvas SVG pan/zoom + breadcrumb + layer toggles + zoom trong canvas; legend gắn dòng provenance khai báo module layer là structural demo.
- Inspector 4 tab: Selection / Impact (dossier snippet song song) / Memory (dossier đa note với flow Verify → Refetch → 409) / Live.
- Journal bar đáy: status pills ok/errors + search dùng chung; click chip tape hoặc hàng Live → drill-through select() node khớp args.name + flash + ping ring; không khớp node → hint tra journal.

**Ngôn ngữ thị giác**: Token Shibumi; node phân lớp dữ liệu nhìn được: repo primary-soft, symbol surface, module nền surface với border ĐỨT NÉT (structural, không phải data-bound); candidate edge danger đứt nét có tag detector theo dataset; anchor dot 3 màu valid/modified/broken tổng hợp theo trạng thái xấu nhất của các note trên file đó. Minimap của v1 (150×92, kéo viewport rect để pan) đã cắt ở v2; ping ring + flash dùng requestAnimationFrame, prefers-reduced-motion tắt pulse. localStorage key `eg-canvas-theme` của v1 hợp nhất về `eg-theme` chung bộ ở v2.

**Key components**:

- fitView fit-to-content lúc boot (tính transform từ bbox NODES) + nút Fit; minimap kéo-pan đồng bộ applyView (xác nhận trong phiên: boot sequence `buildGraph → buildMini → fitView → render*` tại `mockup.html:592-604`).
- Legend có dòng provenance khai báo module/containment là structural demo cần endpoint mới.
- Palette Ctrl+K: role listbox/option, ArrowUp/Down roving, Tab-trap, Enter chọn, trả focus về nút mở (xác nhận trong phiên: `mockup.html:569-583`).
- Memory flow đúng contract: touch → "anchored just now · pending re-resolve" (dot giữ warning) → Refetch /api/memories (demo) → mới valid + dot xanh; kèm nhánh 409 `{ok:false,error}`.
- Journal drill-through journal→canvas; chip tape và hàng Live cùng cơ chế.

**Data binding**: Audit tự chạy của spec: mọi property được tham chiếu của SUMMARY/REPO/REUSE/CAND tồn tại trong literal; `graph_calls: 906` đã thêm vào SUMMARY. /api/summary (theo spec: `server.rs:219`): total_calls 1284, reliability 99.3%, p50 0.8ms (graph 0.7), saved 412k/305k, sessions 18/66.7%; agent_aggs demo khớp tổng (zcode 846/6 err + claude 438/3 = 1284/9). /api/reuse (`server.rs:232`): total_candidates 38, repo pool 21, preventable_loc 1240 est, prewrite 112, avg_callers 6.4, detector_counts D1:14/D1L:5, resolved_twins 3/6, candidate parse_token→normalize_token đầy đủ. /api/memories (`server.rs:251`): note 42 sample nguyên vẹn + 2 note demo cùng shape (57 broken, 51 valid); POST /api/memory/touch (`server.rs:409`) cả hai response shape 200 lẫn 409. Journal đúng shape graph_recent_queries[0]/SSE với exit-code map đúng (0 ok, 1 empty, khác = "exit N").

**Trade-offs**:

- Topology canvas cần endpoint chưa tồn tại (symbol-inventory + containment edges đọc từ index.db) — mockup không bịa endpoint: layer module gắn nhãn structural demo (đứt nét + dòng provenance trong legend) và cam kết fallback suy giảm về repo node + cặp candidate + memory anchor khi thiếu endpoint.
- Fit-to-content ở 1440px cho k≈0.79 nên chữ node hiệu dụng ~10px — mọi badge/label thấy ngay từ đầu nhưng đọc to thì zoom.
- p95_ms, avg_ms, saved_dollars, worktrees không hiển thị vì sample payload không có giá trị — không bịa số.
- Demo stream là interval 2.2s mô phỏng SSE; esc() đầy đủ áp dụng 54 call site.
- 607 dòng, nhỉnh ~1% so khung 300–600 của brief do các fix major (drill-through, đa note, palette a11y, live-state, escape-hardening); v2 giữ đúng mốc 607 — cắt minimap/ticker tape nhưng chỗ đó được a11y fixes chiếm lại.

**Best-fit**: Developer điều tra không gian mã nguồn: truy vết một lời gọi journal vừa xảy ra về đúng node, suy luận impact quanh một symbol qua candidate edge và anchor note gắn trên cùng vùng hình học, re-anchor memory theo đúng contract touch→re-resolve. Kém phù hợp nhất cho bulk triage toàn pool 38 candidates và cho báo cáo tổng hợp thời gian — các tác vụ đó nên dùng option dạng bảng/editorial.

**Khác biệt cốt lõi so với 4 phương án còn lại**: Duy nhất theo mô hình canvas không-trang hub-and-spoke; drill-through journal→canvas + ping ring là đặc tính không phương án nào có (minimap của v1 đã cắt ở v2, signature canvas còn nguyên). Chủ ý de-scope bulk triage (nhường cho 1/2) và temporal (nhường cho 5). Là phương án duy nhất gắn nhãn tường minh phần structural demo và cam kết fallback suy giảm khi thiếu endpoint.

### 2.4 Option 4 — Command Workbench

**File**: `option-4-command-workbench/mockup.html` (841 dòng; v1 908 → 841, −7%).

**Surface Mode**: Mode C — Shoji / Studio Hybrid (create & inspect); file tự khai ở chrome comment (`mockup.html:73`).

**Minimalism v2** (908 → 841 dòng, −7%; verify Pass cycle 2): briefing 13 → 3 delta rows; digest 12 → 4 số; palette index 19 → 14 item; focus trap vòng đa element thật (thay single-focusable); staged buffer cap 100; `@supports` fallback cho `light-dark()`; phản hồi "Copied" nhìn thấy được; fix Critical: palette input mất focus indicator (đã sửa); phòng ngừa class bug `[hidden]` cùng họ với O2.

**Ý tưởng một câu**: Dashboard palette-first mở sẵn ngay lúc boot (Cmd+K): Bench briefing 13 hàng + index nhóm theo Agents/Repos/Candidates/Notes/Invocations, 3 task panel docked theo trọng số task-flow, mọi dossier được chọn từ list (không hard-code); chrome còn lại digest segmented theo pillar và statusbar phím tắt.

**IA**: 4 lớp:

1. Palette overlay MỞ MẶC ĐỊNH lúc boot (`openPalette()` trong init — xác nhận trong phiên: `mockup.html:905` kèm comment "palette-first boot"): briefing 13 hàng (thêm hàng agents từ agent_aggs) + index 19 item sinh từ payload: 5 Actions + 3 Agents + 2 Repos (kèm số worktree) + 3 Reuse candidates + 3 Memory notes + 3 Recent invocations; chọn item Agent/Repo còn set filter cho journal.
2. Workbench 3 panel docked với trọng số task-flow (journal 1.3 = 537px / dup 1.15 = 475px / mem 0.9 = 372px @1440), mỗi panel có flow breadcrumb (pool → twin review → resolve / action center → verify → refetch / tail → filter → inspect) và scope-chip động; Duplicate Review và Memory là list-then-dossier.
3. Live Journal: agent rollup clickable từ agent_aggs, filter pills sinh động (kể cả worktree qua repo_main_name), staged buffer: entry /events mới vào banner "Apply N new (n)" thay vì prepend tự động.
4. Statusbar 5 hint đều hoạt động: cmd k palette · alt 1–3 panels · esc đóng panel last-focused · n apply staged · `/` search; chip live 3 trạng thái live/poll/off, age tính từ generated_at.

**Ngôn ngữ thị giác**: Token Shibumi nguyên vẹn; Georgia display / system sans / ui-monospace. Light + dark qua `[data-theme]` VÀ `@media (prefers-color-scheme: dark)` (không cần JS pre-paint). Điểm thị giác mới: flex weights phân bổ không đều; jrow compact 7 track (54/48/84/minmax(120,1fr)/56/48/44, min-width 490) trong `.journal-scroll`; pick-list rows is-on viền primary; flow-hint mono 9.5px; apply-banner dashed; chip live 3 màu. Badge exit_code là text span màu thay vì chip.

**Key components**:

- Palette boot + focus trap (Tab giữ ở input) + trả focus về trigger; phím `/` cũng mở palette.
- Digest segmented 5 pillar-scope: all / graph / value / agents / sessions.
- Duplicate Review selection-driven: pick-list 3 candidates (D4 hiện "structural" vì overlap_pct null), meta-line tách bạch "global sample D1 ×14 · D1L ×5" vs "repo pool 21 · cap 25/repo", snippet song song, CTA thật: Copy canonical site (clipboard + fallback) và Copy CLI `eg callers <canonical>` (cú pháp từ `eg callers --help`), dòng read-only "resolution happens via a code merge in your editor or CLI".
- Memory Ref Health: Action Center counts tính từ notes[] (1 modified / 1 broken / 1 valid); dossier touch 200 không đổi chip (chỉ anchored_at + touch-log); nút Refetch với semantic đúng; nút Simulate 409 hiển thị `{ok:false, error}`.
- Live Journal staged apply: phím n hoạt động; không mất vị trí scroll khi agent đang chạy.
- Empty state khi đóng hết panel; theme toggle nhất quán với prefers-color-scheme.

**Data binding**: /api/summary: saved_dollars 6.18 est (rate $15/MTok trong tooltip), agent_aggs (zcode 742/2 err, codex 318/5, claude 224/2 — tổng khớp chính xác 1284 calls · 9 errors · 906 graph_calls · 412k tokens), repo_aggs /ws/app + /ws/engram. /api/reuse: detector_counts relabel "global sample", total_candidates 38, repo_candidate_pools {/ws/app:21, /ws/engram:9}, resolved_twins 3/6, 3 candidates (candidates[0] là sample gốc parse_token→normalize_token). /api/memories: 3 notes (allowlist-refs modified là sample gốc; flock-build-lock valid; schema-gate-exit-code broken — echo gotcha thật từ eg mem query). POST touch 200/409 đúng contract, không gán status mới cho ref. CLI strings lấy từ `eg callers --help` / `eg mem touch --help` chạy trong phiên của spec. Không bịa endpoint.

**Trade-offs**:

- 841 dòng (v1 908); 2 candidates demo + 2 notes demo + repo engram + toàn bộ agent_aggs là dữ liệu demo cùng field shape thật (chỉ candidates[0], notes[0], repo app và entry journal đầu là sample gốc) — bắt buộc để palette/Action Center không sập trên dữ liệu nhiều hơn 1 mục.
- Simulate 409 và click-chip-đổi-trạng-thái-live là nút demo có nhãn rõ; production bind vào EventSource onerror thật (file:// chạy synthetic).
- Grid 1440/3-panel được verify bằng số học (node script: FITS_NO_SCROLL=true) chứ không render headless; `.journal-scroll` là an toàn dự phòng cho cửa hẹp hơn.
- Refetch demo luôn trả nguyên trạng thái cũ (trung thực: refetch không tự đổi status nếu code không đổi) — nhánh flip chip có code nhưng không trigger được trong demo tĩnh.
- Focus trap v1 là single-focusable — đủ cho dialog một-input nhưng không phải trap vòng đa element; v2 đã thay bằng trap vòng đa element thật.
- localStorage key `eg-theme` — từ v2 là key chung của cả 5 phương án (key sản phẩm thật).
- Đã chạy: node --check, DOM-stub boot harness (paletteOpen=true, briefing rendered, 3 panels open), grid arithmetic @1440, perl banned-chars (0), tag-balance, escapeHtml 33 chỗ. KHÔNG chạy screenshot.

**Best-fit**: Developer vận hành bằng bàn phím, phiên ngắn một-luồng-việc: mở máy là palette sẵn với briefing tổng quan, gõ vài chữ nhảy thẳng tới candidate/note/agent/repo (kèm set filter), xử lý trong dossier selection-driven, tail journal staged không mất vị trí scroll. Kém phù hợp khi cần so sánh song song nhiều candidate cùng lúc hoặc xem biểu đồ theo thời gian — chủ ý đổi độ rộng lấy chiều sâu một mục.

**Khác biệt cốt lõi so với 4 phương án còn lại**: Palette là cửa chính mở sẵn lúc boot — option 3 chỉ có jump-overlay Ctrl+K, 3 phương án còn lại không có palette làm entry point. Staged-apply banner ("Apply N new (n)") là đặc tính của riêng nó. Mô hình on-demand 3 docks đối lập trực tiếp với everything-visible (1) và selection-driven stream (5); độ sâu một mục đổi lấy việc không song song và không timeline.

### 2.5 Option 5 — Chronicle Timeline

**File**: `option-5-chronicle-timeline/mockup.html` (1004 dòng — dài nhất trong 5; v1 1010 → 1004, −1%).

**Surface Mode**: Mode C — Shoji / Studio Hybrid (create & inspect): stream + record rail, detail thành overlay drawer khi hẹp.

**Minimalism v2** (1010 → 1004 dòng, −1%; verify Pass cycle 2): taxonomy lane đổi thành build/query/memory/session/schema đúng concept (graph/prewrite/reuse thành subtype của Query; thêm demo event session + schema); row ornaments 5 → 2; filter 12 → 10 (agent filter khôi phục dạng chip); detail ≤5 section; Error state cấp view + Retry; live tick prepend không rebuild (giữ focus/scroll); fix `[hidden]` display; sample align với KPI sessions (12/18 = 66.7%).

**Ý tưởng một câu**: Toàn dashboard là time-stream dọc kiểu "git log cho toàn hệ thống" với kỷ luật provenance: 5 lane event (Graph, Build, Pre-write, Reuse, Memory) trên retained journal window, mỗi event derive có nhãn mốc thời gian nguồn, detail pane là record inspector có chuỗi "Nearby on this repo (±45m)" để nhảy chéo event, chip LIVE có state machine thật.

**IA**: 3 cột 236|1fr|400:

- Rail trái — 4 section đều render từ data object bằng JS (một đường bind duy nhất): Ledger (SUMMARY), Workspaces (REPOS: calls, graph, err, tok, mem R/W 22/9, worktree split main 606 + perf-lab 34, reuse hit; click để filter stream), Agents (AGENTS), Engine (REUSE) — pool 38 và preventable LOC est đứng RIÊNG, hàng "Detector (sample floor)" chỉ có D1 14 · D1L 5 và là NÚT LỌC stream, rail-note ghi rõ sample floor ≠ pool.
- Cột giữa — timeline: 5 lane pill (+Build màu neutral, tách lệnh hạ tầng khỏi graph query); Errors-only đếm theo `exit_code !== 0`; zoom Day/Hour có nhãn "groups the retained 50-entry window"; day header tự sự dạng ledger derived ("18 events · 1 failed · graph 7 · prewrite 2 · reuse 7 · memory 2 · top callers") + note "single day in retained window".
- Detail record inspector — 5 template + section "Nearby on this repo (±45m)" (3 event cùng repo lân cận, click nhảy chọn) + "Raw payload" JSON đúng shape server; memory template có "Touch semantics" explain + 3 nút: Verify & re-anchor (POST, chỉ advances anchored_at), Refetch /api/memories (re-resolve ref), Simulate 409 (`{ok:false,error}`).
- ≤1280px: detail chuyển thành overlay drawer (.shell.dt-open) thay vì biến mất.

**Ngôn ngữ thị giác**: Token Shibumi 1:1; 5 lane map intent hiện có (graph=info, build=neutral, prewrite=warning, reuse=primary, memory=success, error=danger override). Chip LIVE là button 3 state class is-live/is-reconnect/is-paused (dot đổi màu, pulse chỉ khi live — xác nhận trong phiên: `mockup.html:35-40`); ageChip thêm .stale (amber) qua watchdog 1s khi >30s không event mà không pause. Chip st-unverified neutral mới cho `resolved_refs[].status = 'unverified'`. Georgia cho day-title/detail-title, mono cho giờ/symbol/số liệu; chỉ còn 1 inline style là width data-driven trên .fill.

**Key components**:

- Chip LIVE 3 state + stale alarm + pause click (xác nhận trong phiên: `mockup.html:995-1000`).
- SSE simulation state machine: tick 9s, drop link mỗi tick thứ 5 → RECONNECTING → stale alarm → reconnect; EVENTS cap 50 đúng take(50) (xác nhận trong phiên: `mockup.html:968-994`).
- Day header ledger derived từ group, không phải prose editorial.
- Detail 5 template + Provenance section ghi mốc anchor + Nearby ±45m jump.
- Touch contract đúng chuẩn: 200 chỉ cập nhật anchored_at (flag nội bộ); ref_status/resolved_refs chỉ đổi khi Refetch /api/memories; broken giữ nguyên broken; 409 demo được.
- Rail data-rendered; detector filter là nút lọc stream theo detector thật (dataset).

**Data binding**: Không bịa trường hay endpoint. Error demo dùng exit 2 (invalid usage) và exit 5 (schema skew) theo exit codes thật của eg; badge in động "exit " + code; isErr = `exit_code !== 0`. Scans generate từ REPOS[].last_reuse_scan{ts, result_count, limit_capped, prev_result_count, prev_ts}; twin-surfaced events anchor vào last_reuse_scan.ts (convention client-side được khai báo vì candidates[] không có surfaced-at field); twin-reused anchor vào last_reused_ts. candidates[0] giữ nguyên số liệu sample (D1, parse_token→normalize_token, overlap 83, callers 14, loc 22); 3 mục còn lại demo đúng field shape (D3 có overlap_pct null → detail vẽ chú thích thay vì bar bịa). Notes 42/41/39 với resolved_refs valid/modified/broken/unverified. Summary giữ bộ số sample + sums tự kiểm bằng harness (640+388+256=1284; 512+244+150=906; err 9; 606+34=640; agents 512+448+324=1284).

**Trade-offs**:

- 1004 dòng (v1 1010), trên khung ~300–600; phần tăng (786→1010) đến từ các fix bắt buộc: 4 candidates với snippet, context chain ±45m, state machine SSE, 3 nút touch/refetch, rail data-rendered — v2 cắt nhẹ còn 1004 nhưng giữ đủ các fix đó.
- Render headless trong browser KHÔNG chạy được (không Chrome/puppeteer/playwright/jsdom — đã check require.resolve); thay bằng node --check + DOM-stub harness (`node /tmp/chronicle-boot.js` → "HARNESS_OK: boot + 29 event details + flows verified") + python scan (0 emoji/sparkline/URL ngoài/personal path; CSS brace 173/173; đúng 1 inline style).
- Demo values kê khai tường minh: p95_ms 2.4, avg_ms 0.9, graph_p95 2.1, saved_dollars 1.24 + graph_saved_dollars 0.92 theo $3/MTok nội bộ — cần owner CHỐT MỘT TỶ LỆ CHUNG vì option 1/4 đang kể $6.18 cho cùng 412k tokens.
- Twin-surfaced là convention: anchor theo last_reuse_scan.ts là implementable từ trường có thật nhưng nếu backend thêm field surfaced-at thì mock cần đổi anchor.
- Zoom Day/Hour chỉ là GROUPING của retained 50-entry window — graph events sâu hơn 50 entry không thấy (giới hạn dữ liệu thật, không phải thiếu tính năng).
- Detector filter là client-side trên sample floor trong window; pool-wide per-detector chưa có số — chỉ hiện khi backend xuất trường mới.
- Nearby ±45m là derive client-side từ EVENTS (đúng dữ liệu có trong window), không phải endpoint.

**Best-fit**: Soát theo dòng thời gian và điều tra hệ thống đang tiến hóa như đọc git log — audit console vận hành nhiều agent nhiều repo: provenance trung thực (mọi derived event ghi rõ mốc nguồn, sample vs pool phân lớp), memory actions tuân thủ contract thật (touch chỉ re-anchor, refetch mới re-resolve, broken không tự lành), sẵn sàng cho mạng thật (chip state machine + stale alarm). Kém phù hợp nếu cần phân tích aggregate sâu (Ledger chỉ là rail phụ).

**Khác biệt cốt lõi so với 4 phương án còn lại**: Duy nhất theo mô hình time-stream day-grouped + record inspector selection-driven; Nearby context chain ±45m và provenance labels là đặc tính không phương án nào có. Chip LIVE state machine + stale alarm bắt weakness mạng thật mà chip tĩnh của các phương án khác không có. Day header là ledger rollup derived (khác prose editorial của option 2) và Engine là bộ lọc sample-floor thay vì stat chip (khác option 1).

## 3. Bảng so sánh theo trục

| Trục | 1 Mission Control | 2 Editorial Journal | 3 Graph Canvas | 4 Command Workbench | 5 Chronicle Timeline |
|---|---|---|---|---|---|
| Mô hình IA | Console 4 band cố định "mọi thứ hiện cùng lúc", không scroll trang | Tài liệu báo chí cuộn dọc theo desks, folio sticky | Canvas node-map hub-and-spoke không trang, điều tra quanh một selection | Palette overlay mở sẵn lúc boot + 3 panel docked on-demand | Time-stream dọc day-grouped + record inspector selection-driven |
| Surface Mode | B — Shibumi Utilitarian | A — Swiss / Zen Editorial | C — Shoji / Studio Hybrid | C — Shoji / Studio Hybrid | C — Shoji / Studio Hybrid |
| Mật độ thông tin | Rất cao: 4 KPI + feed làm tâm (evidence trong inspector) trong 1 viewport | Trung bình: nhịp đọc editorial, pull-quote, margin annotation | Trung bình: canvas chiếm chủ đạo, rail/inspector phụ | Trung bình: briefing 3 delta + palette 14 item + 3 panel, mỗi panel một luồng việc | Trung bình–cao: 5 lane + rail + day header rollup |
| Primary job | Ambient watch realtime nhiều agent/repo/worktree | Catch-up đọc lại bình tĩnh và ra quyết định | Điều tra không gian mã nguồn và impact quanh symbol | Phiên keyboard ngắn, một-luồng-việc | Audit retrospective theo dòng thời gian, provenance-first |
| Ngôn ngữ thị giác | All-mono terminal (floor 10px, ramp ≤5 cỡ), tabular-nums, hue-coded | Georgia editorial, 1 display hero 44px, pull-quote bất đối xứng, cột 62ch | Node-map phân lớp, border đứt nét structural, ping ring, keyboard pan/zoom + LOD | Palette overlay + flex weights (1.3/1.15/0.9) + pick-list + apply-banner | Spine 5-lane, day-header ledger derived, chip LIVE 3 state, Georgia/mono |
| Đánh đổi chính | Không dossier sâu; filler demo khớp tổng; feed cap 50 in-window | Mất kịch tính catch-up (không baseline last-visit); drill-in yếu hơn inspector; scroll dài | Cần endpoint chưa tồn tại cho topology; chữ node ~10px khi fit-to-content | Không so sánh song song; không biểu đồ thời gian; nhiều mục demo | Aggregate sâu yếu; derive dựa convention anchor; file dài nhất (1004 dòng) |

Ghi chú shared design system (hội tụ có chủ đích sau minimalism v2): cả 5 dùng chung 1 bộ 25 semantic tokens Shibumi (nguồn `src/dashboard/assets/style.css:1-60`), chip vocabulary thống nhất 1 class `.chip` + intent variants, theme key chung `eg-theme`, type ramp ≤5 cỡ (O2: ≤9 editorial roles), text floor 10px (muted ≥11px), `prefers-reduced-motion` đầy đủ (CSS + JS), `focus-visible` cả 2 theme, control boundary ≥3:1, reflow breakpoint ≤900px. Hạ tầng chung này không tính vào trục phân hóa — sự khác biệt thật nằm ở IA, mật độ/ngữ pháp thị giác và primary job.

## 4. Chọn theo nhu cầu

Không có thứ tự ưu tiên; mỗi dòng là một ánh xạ tình huống → phương án.

| Tình huống sử dụng | Phương án phù hợp | Lý do ngắn |
|---|---|---|
| Mở dashboard thường trực cả ngày để giám sát nhiều agent/repo/worktree, bắt lỗi ngay khi xảy ra | 1 | Feed live làm tâm + các bảng tín hiệu hiện cùng lúc (v2: 4 KPI, evidence trong inspector); duyệt event bằng bàn phím không cần chuột |
| Máy lần đầu chạy eg, chưa có journal (cần thấy trạng thái rỗng đúng contract) | 1 | Duy nhất có scenario FIRST-RUN với quy tắc null rõ: dash thay số, không bịa 0 |
| Quay lại sau vài giờ/ngày, muốn đọc tổng quan bình tĩnh rồi quyết định xử lý twin/anchor | 2 | Nhịp đọc editorial; twin dossier hoán đổi được ngay trong trang; anchor ledger hiện trạng thái drift |
| Điều tra "thứ này nằm ở đâu trong mã nguồn": từ event → node → impact quanh symbol | 3 | Drill-through journal→canvas + flash/ping; candidate edge và anchor note cùng vùng hình học |
| Làm việc bằng bàn phím, mỗi phiên một việc: nhảy tới candidate/note/agent rồi xử lý trong dossier | 4 | Palette mở sẵn lúc boot, index 14 item sinh từ payload; staged journal không mất vị trí scroll |
| Audit hệ thống theo dòng thời gian như đọc git log; cần biết mỗi số derive từ mốc nào và ngữ cảnh event lân cận | 5 | Time-stream 5 lane + provenance labels + Nearby ±45m; chip LIVE state machine sẵn sàng cho mạng thật |
| Bulk triage nhiều candidate theo kiểu bảng, so sánh nhiều hàng cùng lúc | 1 hoặc 2 | Option 1 có R1 + evidence trong inspector; option 2 có Bureau với dossier hoán đổi — option 3/4/5 tự de-scope job này |
| Phân tích aggregate số liệu sâu (điểm yếu tự khai của 5) | 1 hoặc 2 | KPI strip (1) và desks với stat strip (2) giữ aggregate ở vị trí chính |

## 5. Caveats và giới hạn

1. **Độ khác biệt giữa 5 phương án (kiểm chứng độc lập)**: vòng v1 — giám khảo độc lập xác nhận cả 10 cặp đạt ngưỡng khác nhau ở ít nhất 2/3 trục (IA, primary job, ngôn ngữ thị giác), 0 vi phạm; đối chiếu spec với file bằng 24 grep probe, spot-check 5 marker khớp (Phụ lục A). Sau minimalism v2, kiểm chứng độc lập chạy lại: 10/10 cặp khác nhau đủ 3/3 trục (IA / visual+density / primary job), 0 vi phạm, signature của từng phương án còn nguyên. Vì cả 5 dùng chung token Shibumi (ràng buộc repo), phân hóa thật nằm ở typography/mật độ/ngữ pháp bố cục; hạ tầng chung hội tụ có chủ đích (mục 3) không tính là hội tụ thiết kế.
2. **Watch-item hội tụ (cập nhật sau v2)**: cặp gần nhau nhất giờ là 2↔5 — cùng mô hình cuộn dọc, tách được nhờ grouping topical-vs-temporal và signature pull-quote/ledger (2) vs lanes/Nearby ±45m (5); nếu có pass cắt tiếp, đây là chỗ đầu tiên có nguy cơ hội tụ IA. Cặp 1↔5 và 4↔5 của vòng v1 vẫn khác biệt rõ (everything-visible vs time-stream; palette-first docks vs scroll stream) — token chung không tính là hội tụ. Giữ chặt 2 đặc tính riêng khi chỉnh tiếp: staged-apply banner (của 4) và Nearby context chain (của 5).
3. **A11y findings đã xử lý và verify cycle 2 độc lập (thay caveat phạm vi của vòng v1)**: các runtime bug thật đã fix — (a) O2 wire filter bị `.entry{display:grid}` ghi đè `[hidden]` (Critical, đã fix; O4/O5 phòng ngừa cùng class bug); (b) O4 palette input mất focus indicator (Critical, đã fix); (c) focus ring bị `outline:none` cắt ở O1/O3/O4/O5 (đã sửa); (d) control boundary <3:1 ở cả 5 (đã sửa bằng ink text-muted); (e) ARIA state filter/listbox/role=button (đã bổ sung); (f) reflow 320px/200% ở O3/O4 (breakpoint ≤900px); (g) O5 live re-render mất focus (chuyển sang prepend). Contrast tối nhất hiện là muted/elevated dark 4.60:1 — margin mỏng, watch item khi chỉnh token.
4. **Mockup dùng dữ liệu mẫu**: các file dùng samplePayloads và filler demo được kê khai tường minh trong từng file (Colophon của option 2, meta-line của option 4, rail-note/legend/provenance của option 3 và 5). Các tổng số demo được thiết kế khớp KPI (1284 calls / 9 err / 412k tok / 906 graph) nhưng không phải dữ liệu thật của `.engram/` trên máy người dùng; khi mở mockup, số liệu không phản ánh hệ thống thật. O5 đã align sample với KPI sessions (12/18 = 66.7%).
5. **Chưa render kiểm tra trực quan trên trình duyệt**: máy chỉ có Safari, không có Chrome/Chromium/Edge/Brave CLI headless. Mọi kiểm chứng của bản v2 là code-level + deterministic scans (node --check, perl banned chars, tag balance, contrast tính toán, wc -l); vòng v1 đã chạy thêm DOM-stub harness (option 4: paletteOpen=true + 3 panels; option 5: "HARNESS_OK: boot + 29 event details") và grid arithmetic @1440. Không có screenshot; nhận định layout dựa trên grid math, không phải render thật.
6. **Chưa triển khai code**: 5 mockup là thiết kế; không có dòng nào nằm trong `src/dashboard/assets/`. Một số tính năng cần backend mới: topology canvas (option 3) cần symbol-inventory + containment edges đọc từ index.db; pool-wide per-detector counts chưa có số; zoom/báo cáo thời gian sâu hơn retained 50-entry window cần API mới; baseline last-visit cho khung catch-up (option 2 đã bỏ) cũng cần backend.
7. **Cần quyết định owner trước khi production**: (a) tỷ lệ quy đổi dollars/MTok chưa thống nhất — option 1/4 kể $6.18 cho 412k tokens ($15/MTok), option 5 dùng $3/MTok nội bộ ($1.24 + $0.92) và tự chỉ ra sự không nhất quán này; (b) anchor mốc "twin surfaced" của option 5 là convention client-side theo last_reuse_scan.ts — nếu backend thêm field surfaced-at thì mock cần đổi anchor; (c) browser floor: O4 + O5 dùng `light-dark()` (Baseline 2024) — O4 có `@supports` fallback, O5 cố ý bỏ fallback (browser cũ mất màu, không mất chức năng); nếu port vào `src/dashboard/assets/` cần chốt browser floor.
8. **Bản v1 lưu làm archive**: 5 file phẳng `option-1..5-*.html` cùng thư mục hiện giữ BẢN v1 (trước minimalism) làm archive — dùng để so sánh A/B v1 vs v2; mọi tham chiếu trong báo cáo này trỏ tới `mockup.html` (v2) trong từng thư mục `option-*/`.
9. **Tính hợp lệ của 5 file mockup**: đã kiểm trong phiên v1 — cả 5 tồn tại, là HTML hoàn chỉnh (đọc đầu/cuối bằng Read, có `<!DOCTYPE html>`, `<html>`, `<body>` có nội dung, `</body></html>`); phiên cập nhật v2 chạy lại wc -l và tag balance với kết quả nhất quán.

## Phụ lục A — Kiểm chứng đã chạy trong phiên này

| Kiểm tra | Lệnh / cách chạy | Kết quả |
|---|---|---|
| Tồn tại + số dòng | `wc -l docs/designs/dashboard-redesign/option-*/mockup.html` | v1: 871 / 908 / 607 / 908 / 1010 — khớp spec từng phương án; v2 (hiện hành): 769 / 835 / 607 / 841 / 1004 |
| Tag HTML | `grep -c '<body'` và `grep -c '</html>'` trên từng file | Mỗi file: đúng 1 `<body>`, đúng 1 `</html>` |
| Đầu file | Read 50 dòng đầu từng mockup | Đều có `<!DOCTYPE html>`, `<html>`, `<head>` với token Shibumi light/dark; theme pre-paint bằng script (1: `eg-mc-theme`, 2: `eg-theme`, 3: `eg-canvas-theme`) hoặc CSS-only `@media prefers-color-scheme` (4, 5). v2: cả 5 dùng key chung `eg-theme`; 4 và 5 dùng `light-dark()`, 4 có `@supports` fallback |
| Cuối file | Read ~42 dòng cuối từng mockup | Đều có script init + `</body></html>`; thấy `setFirstRun` (1, dòng 830-856), touch 200/409 (2, dòng 878-891), boot `buildGraph/buildMini/fitView` (3, dòng 592-604), `openPalette()` trong init dòng 905 kèm comment "palette-first boot" (4), SSE state machine + stale watchdog + EVENTS cap 50 (5, dòng 968-1000) |
| Marker đặc trưng | `grep -c 'FIRST-RUN'` (1) → 2; `grep -ci 'folio'` (2) → 14; `grep -c 'minimap'` (3) → 2; `grep -c 'openPalette'` (4) → 5; `grep -c 'Nearby'` (5) → 1 | Khớp probe của giám khảo; tính năng đặc trưng có thật trong file. Ở v2: `minimap` (3) về 0 do đã cắt; các marker còn lại giữ nguyên giá trị |
| Nguồn memory_* | Read `src/dashboard/data/summary/payload.rs:236-253` | Xác nhận repo_agg serialize `memory_calls` / `memory_errors` / `memory_reliability_pct` / `memory_reads` / `memory_writes` / `memory_read_hit_rate_pct` tại dòng 242-247 |
| Nguồn sample floor | Read `src/dashboard/data/reuse.rs:464-495` | Xác nhận comment "Sample-floor counts..." và detector_counts đếm trên candidates_json (dòng 467-472); 3 cờ estimated: `preventable_loc_estimated` (480), `avg_callers_estimated` (489), `detector_counts_estimated` (492) |

Các nhận định còn lại trong báo cáo (IA, trade-offs, best-fit, số liệu demo) được tổng hợp từ 5 spec do yêu cầu cung cấp; những chỗ chỉ có trong spec (không đọc trực tiếp trong phiên) được ghi "theo spec" kèm đường dẫn nguồn mà spec trích dẫn. Trạng thái v2 được xác minh bằng 2 vòng review + verify cycle 2 độc lập cho từng option (kết quả: cả 5 Pass) và bộ quét deterministic liệt kê ở mục 5.
