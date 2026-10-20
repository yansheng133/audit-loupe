# 第 1 層後端實作計畫（給 Sam 審，尚未實作）

寫作：2026-10-21。依據：
- `docs/change-map-requirements.md` 第 3 節（5 秒呈現）、第 5 節（v5 頁面）。
- `docs/lab/report-ui-facts.md`（第 0 層實測數字）。
- `docs/lab/report-rotation-live.md` 與 PR #11（收集元件回報「沒有收集」）。

這份只是計畫，**還沒有寫任何程式**。

## 一句話結論

開頁要讀的東西，改成在收資料時就算好、存成三張小表：物件每日狀態、狀態轉變、每 10 分鐘一格的時間列。再加上讀寫分開、回應壓縮，用一支 `/api/changemap` 一次拿回整頁。建議分 5 個後端 PR，依序做；最先做的「讀寫分開＋壓縮」不改 schema，風險最低、馬上量得到效果。

## 名詞

| 名詞 | 意思 |
|---|---|
| 稽核狀態 | 每個物件在某一天的結論：要看、看不清、看不到、清楚（第 5 節 (1)） |
| 彙總表 | 收資料時順便累加的小表；開頁只讀它，不掃原始紀錄 |
| 影子表 | 重新計算時先建在旁邊的一份新表，算完一次換上，畫面不會看到算一半的狀態（`rulestate.rs` 現在就這樣做） |
| 游標分頁 | 翻頁時帶「上一頁最後一筆的位置」，不用 OFFSET，資料多也不會變慢 |
| 10 倍基準 | 第 0 層量到最大的一天（10/18，72,244 筆 events）的 10 倍，約 **72 萬筆／天** |

## 0. 第 0 層給的數字（設計的前提）

| 項目 | 實測（lab） | 10 倍基準 | 出處 |
|---|---|---|---|
| 一天的 events | 66,541（10/20）～72,244（10/18） | 約 72 萬 | report-ui-facts E |
| 一天被動過的物件 | 下游 607、local 4（另有 82 個執行者不明） | 約 6,000＋800 | K |
| 有操作的時段 | 每 10 分鐘：144/144；每分鐘：937/1,440 | 同左或更滿 | K |
| 「人」與「未知」的寫入 | 278 筆／天 | 約 2,800 | C |
| 最慢的 API | `/api/clusters` 冷 3.4 秒、熱 0.27 秒；`/api/actors` 0.36 秒 | 隨資料量線性變慢 | H |
| 同時送 8 支 | 排在最慢那支後面，整批 0.7～1.9 秒 | — | H |

結論：
- **一天的「人」與物件數量都很小，可以預先算好。**
- **時間列用每 10 分鐘一格**：每小時一格的話，下游每一格都有東西，看不出差別；每分鐘一格的話一天 1,440 格，太碎。
- **讀取在排隊**：先把讀寫分開，效果最直接。

## 1. 物件的稽核狀態

### (1) 判斷規則（依第 5 節 (1)，請 Sam 確認）

一個物件在某一天（`server.timezone` 的 00:00–24:00）的狀態，依下表**由上往下第一個符合的**決定：

| 順序 | 狀態 | 條件 | 依據寫在畫面上的內容 |
|---|---|---|---|
| 1 | 要看 | 當天有任何一筆中、高風險警示 | 規則代碼、嚴重度、判斷時的設定代碼 |
| 2 | 看不清 | 當天有「事後比對發現的變化」（`versions` 中 `event_id IS NULL AND change <> 'baseline'`），也就是找不到執行者 | 「重新連線後比對」或「該叢集沒收 apiserver audit」（第 0 層 F：local 的情況） |
| 3 | 看不到 | 當天這個物件的紀錄或版本，落在該叢集「沒有收集」的期間內或緊鄰它（前後 1 個時間格） | 缺口的原因與起訖（PR #11 的 `reason`） |
| 4 | 清楚 | 以上都不是 | 每一筆都對得到身分；低風險只當淡色提示 |

- **待決定 1**：「看不到」的定義。沒收到的資料，物件本身也不會出現。建議：
  - 物件層級只標「有紀錄落在缺口邊緣」的物件。
  - 時間列與總覽上方另外顯示叢集層級的「看不到」時段。
- **待決定 2**：物件的鍵要加上 API group。第 0 層 G 發現 `nodes`（Kubernetes Node）和 Longhorn 的 `nodes.longhorn.io` 名稱一樣，現在的鍵分不出來。

### (2) 收資料時怎麼算、存在哪裡

新表 `object_day`：

| 欄位 | 說明 |
|---|---|
| `cluster, day, api_group, resource, namespace, name` | 主鍵。`day` 是 `server.timezone` 的日期 |
| `ops`、`writes` | 操作次數、寫入次數 |
| `slots` | 當天 144 格的寫入次數（迷你條用；壓縮成固定長度的 BLOB，約 300 bytes） |
| `last_ms`、`last_actor_key`、`last_via`、`last_identity_class` | 最後一次 |
| `worst_severity` | 當天最嚴重的警示 |
| `unattributed` | 事後比對發現的變化數 |
| `near_gap` | 是否有紀錄落在缺口邊緣 |
| `state` | 依 (1) 算出的狀態，存起來排序用 |
| `rule_fingerprint` | 判斷用的規則設定代碼 |

更新的時機都在現有的寫入路徑裡，跟 `hourly` 的做法一樣：
- `Store::insert_events`（`store.rs:790`）：每筆計入的紀錄 upsert 一列，累加次數、更新最後一次與最嚴重的警示。已經和下游配對的 Rancher 紀錄不計（跟 `bump` 一樣）。
- `insert_version`／`apply_objects`：沒有對到 event 的變化，`unattributed + 1`。
- `link_since`／`link_pending`：版本後來對到 event 時，`unattributed - 1`。
- 收到「沒有收集」（PR #11 的 `record_gap`）：把那段期間前後 1 格內有紀錄的物件標 `near_gap`。
- 每次更新後重算 `state`；狀態有變就寫一筆轉變（第 2 節）。

### (3) 規則重新計算時

`rulestate.rs` 的 `recompute()` 已經用影子表重建 `stories` 與 `hourly`，所以加一張 `rc_object_day` 一起建、一起換上：
- `worst_severity` 與 `state` 跟著新的判斷改。
- 換上前後比對每列的 `state`，有變的寫一筆轉變，原因是「規則重新計算」。

### (4) 保留期清除

`prune()`（`store.rs:1901`）刪原始紀錄時，同時刪過期日子的 `object_day`。之後改成每週一個資料檔時（3.8），這張表跟著所屬的週檔走。

## 2. 稽核狀態轉變事件

新表 `state_changes`：

| 欄位 | 說明 |
|---|---|
| `id` | 遞增 |
| `cluster, day, object_key` | `object_key` 為空字串時，代表叢集層級（收集狀態） |
| `at_ms` | 什麼時候變的 |
| `from_state, to_state` | 例如 清楚 → 要看・中 |
| `cause` | `event`（某筆紀錄）、`unattributed`（事後比對）、`gap`（沒有收集）、`recompute`（規則重新計算）、`collector`（收集元件恢復正常） |
| `ref_id` | 對應的 event id、version id、gap id 或 recompute id |

- 叢集層級的轉變（例如「rke2 收集 正常 → 看不到」）由 `record_gap` 與心跳恢復時寫入。
- 事件分頁就是依時間倒序讀這張表，加上游標分頁。

## 3. 時間列彙總

新表 `slot_day`，每叢集、每 10 分鐘一列：

| 欄位 | 說明 |
|---|---|
| `cluster, slot_ms` | 主鍵（格子的開始時間） |
| `writes, alerts_mh, unattributed` | 寫入、中高風險、事後比對 |
| `not_collected_ms` | 這格裡「沒有收集」的毫秒數（0～600,000） |
| `worst_state` | 這格的狀態：看不到（有缺口）＞ 要看 ＞ 看不清 ＞ 清楚；**沒有收集畫斜線，不畫成 0** |

- **預設一條合併線**（Sam 10/20 定案）：查詢時把選到的叢集（預設全部）依 `slot_ms` 加總，`worst_state` 取最嚴重的。叢集數 × 144 列，量很小，不必另存合併結果。
- **選了叢集才分開**：同一個查詢不加總，依叢集分組回傳。
- 粒度做成設定 `server.timeline.slotMinutes`（預設 10），改了要重建這張表，所以只在安裝時決定。
- 更新：跟 `object_day` 一起在寫入路徑累加；缺口由 `record_gap` 與心跳缺口填 `not_collected_ms`。

## 4. 一次開頁一個請求的 API

`GET /api/changemap`

| 參數 | 說明 |
|---|---|
| `day` 或 `from`／`to` | 範圍，預設今天 |
| `cluster`（可多個）、`namespace`、`state`、`identity` | 篩選，全部在後端做 |
| `tab` | `overview`（預設）、`relations`、`events` |
| `cursor` | 翻頁用；第一次不帶 |
| `select` | 預設選取的物件；不帶就選排序第一個 |

回應（目標：壓縮前 ≤ 200 KB；以下數字只是格式示意）：

```json
{
  "asOf": {"c-m-26kbxk5f": "2026-10-21T00:37:45Z", "local": "…"},
  "ruleFingerprint": "560d162e0034822d",
  "coverage": [{"cluster": "…", "notCollected": [{"fromMs": 0, "toMs": 0, "reason": "collector-restart"}], "dependsOn": ["local 沒收 apiserver audit log"]}],
  "counts": {"要看": 0, "看不清": 0, "看不到": 0, "清楚": 0, "total": 0},
  "summary": {"unattributed": 0, "humanWrites": 0},
  "timeline": {"slotMinutes": 10, "lines": [{"cluster": null, "slots": [[0, 0, 0, 0, "清楚"]]}]},
  "page": {"rows": ["…100 列…"], "next": "<cursor>"},
  "detail": {"object": "…", "who": ["…"], "evidence": ["…預設選取＋需要注意的前 5 筆…"]},
  "progress": [{"what": "recompute", "done": 0, "total": 0}]
}
```

- **排序與翻頁**：依 `(state 順序, last_ms DESC, 物件鍵)` 排序，`cursor` 就是上一頁最後一列的這三個值。要配合索引 `object_day(day, state_rank, last_ms DESC, …)`。
- **其他分頁**：關係分頁讀 `object_day` 依身分、管道、叢集分組（身分多時合併成群組，上限可設定）；事件分頁讀 `state_changes`。
- 舊的 8 支 API 先保留，等 UI 改完再拿掉。
- **一次開頁＝一筆 Rancher audit 紀錄**（第 0 層 J：每一支經代理的請求都會被記下來）。現在一次開頁是 9 筆。

## 5. 讀寫分開、壓縮、伺服器端分頁與篩選

| 項目 | 做法 | 改的檔 |
|---|---|---|
| 讀寫分開 | `Store` 改成一條寫入連線（現在的 `Mutex<Connection>`）＋ N 條唯讀連線（`SQLITE_OPEN_READ_ONLY`、`PRAGMA query_only=1`），輪流取用。`server.db.readConnections` 預設 4。所有查詢函式改用唯讀連線；`insert_*`、`apply_objects`、`recompute`、`prune` 用寫入連線 | `store.rs`（`Store` 結構與約 30 個查詢函式的第一行）、`config.rs`、chart `values.yaml` |
| 讀取不卡 async | 查詢包進 `spawn_blocking`（現在 `actors`、`clusters` 等是在 async handler 裡直接跑 SQLite） | `api.rs` |
| 回應壓縮 | `tower-http` 開 `compression-gzip`，API router 加 `CompressionLayer`。**要在 lab 確認 Rancher 的 service proxy 會把 `Content-Encoding` 原樣轉給瀏覽器** | `Cargo.toml`、`api.rs` |
| Server-Timing | middleware 記資料庫時間與組裝時間，放進回應標頭 | `api.rs` |
| 篩選與分頁 | 全部在後端做（第 4 節）；前端不再自己算統計 | `store.rs`、`api.rs` |
| 順手修的慢查詢 | `/api/clusters` 改讀一張小表 `cluster_stats`（寫入時維護），不再 `SELECT DISTINCT cluster FROM events`；`/api/actors` 改讀 `hourly` 加總 | `store.rs` |
| 記憶體上限 | chart 在「唯讀連線數 × 每條快取＋程式本身」超過 `resources.limits.memory` 時直接報錯（`fail`） | `deploy/chart/templates/_helpers.tpl` |

## 6. 身分分類（第 1 層的另一半）

第 0 層 B 的結論：groups 認得出 ServiceAccount 與 `system:masters`，但認不出用憑證的元件（`rke2-cloud-controller-manager`）。

- 寫入時多存三樣：`user.groups`、`user.extra` 的鍵名（不存值），以及算好的 `identity_class`。
  - 分類：人、叢集元件、ServiceAccount、Rancher 內部、未知。
  - 存 `events.identity_class` 欄位，並寫進 `object_day.last_identity_class`。
- 分類規則照 report-ui-facts B 的「依據」欄寫成程式（`audit.rs` 取代 `is_human()`），並列出：
  - 已知元件名稱清單（可用 Helm values 增加）。
  - Rancher 系統使用者的判斷：User 物件的 principalIds 有 `system://`。要讀 `users` 表，現在只存了 `principal_ids`，夠用。
- 舊紀錄沒有 groups：重新計算時只能用名稱與 principal 判斷，分不出來的歸「未知」，畫面寫原因。

## 7. 每一項的檔案、schema、測試、5 秒證明、工作量、風險

| # | 項目 | 主要改的檔 | schema／migration | 怎麼測 | 怎麼證明 5 秒 | 工作量 | 風險 |
|---|---|---|---|---|---|---|---|
| 1 | 讀寫分開＋壓縮＋Server-Timing＋`clusters`／`actors` 修正 | `store.rs`、`api.rs`、`config.rs`、`Cargo.toml`、chart | `cluster_stats`（新表，開機時由現有資料建一次） | 既有測試全過；新增「寫入進行中仍可讀」的並行測試 | lab 重量 H：8 支同時打，每支的 Server-Timing ≤ 300 ms | 1.5 天 | 低。唯讀連線看到的是 WAL 的快照，可能落後寫入幾毫秒 |
| 2 | 身分分類 | `audit.rs`、`rancher.rs`、`store.rs`、`rules.rs`（「人」的判斷改用分類） | `events` 加 `groups`、`extra_keys`、`identity_class`（`ADDED_COLUMNS` 機制） | 用 report-ui-facts B 的 22＋2 個身分當測試資料，每個身分的分類與依據都要對 | 不直接影響速度 | 1.5 天 | 中。規則「只看人」的意思會變：`u-d67owytc34` 不再算人，警示數會變，要重新計算並在畫面說明 |
| 3 | `object_day`、`slot_day`＋回填＋重新計算 | `store.rs`、`rulestate.rs`、`store.rs` 的 `prune` | 兩張新表；開機時背景回填既有資料，顯示進度（3.7） | **一致性測試**：任意資料「邊寫入邊累加」＝「從原始紀錄重建」；重新計算前後與從頭重建一致；刪除過期資料後一致 | CI：`seed-demo` 產生 72 萬筆／天，量彙總查詢的伺服器端時間 | 3 天 | 中高。寫入路徑多做事；10 倍資料量下要量收集端延遲。回填 30 天約 200 萬筆，預估幾十秒到幾分鐘 |
| 4 | `state_changes` | `store.rs`、`rulestate.rs`、`api.rs` | 新表 | 每種原因（event、unattributed、gap、recompute、collector）各一個測試 | 事件分頁第一頁 ≤ 100 ms | 1 天 | 低 |
| 5 | `/api/changemap`（一次開頁）＋游標分頁 | `api.rs`、`store.rs` | 索引 | API 測試：篩選、翻頁不重複不漏、`select`、空資料、只有缺口的日子 | **CI 效能測試**：10 倍資料，伺服器端第 95 百分位 ≤ 300 ms，超過就失敗；lab 用正式安裝的 extension 冷開、熱開各 20 次 | 2 天 | 中。回應大小要控制在 200 KB |
| 6 | （UI repo）改用 `/api/changemap` | `audit-loupe-ui` | — | 截圖、捲動，用人的眼睛看 | 第 3.4 (5) 點的 lab 量測 | 另估 | — |

合計後端約 9 個工作天，不含 UI。

## 8. 建議的 PR 切法與順序

1. **PR-1 讀寫分開＋壓縮＋Server-Timing**（不改 schema，只加 `cluster_stats`）。先做，馬上可以在 lab 量到 H 的改善，也是後面每個 PR 量測的基礎。
2. **PR-2 身分分類**。`object_day` 要存身分類別，所以排在它前面。合併後要重新計算一次。
3. **PR-3 `object_day`＋`slot_day`＋回填**。最大的一個，前提是 PR #11（收集元件回報缺口）已合併。
4. **PR-4 `state_changes`**。
5. **PR-5 `/api/changemap`＋CI 效能測試**。
6. UI 改版（另一個 repo），接 PR-5。

每個 PR 都要有：
- 「彙總＝原始紀錄」的一致性測試（從 PR-3 起）。
- CHANGELOG。
- 有動到行為的地方，同步改 `docs/deploy.md`、`docs/design.md`。

## 9. 要 Sam 決定的事

1. 「看不到」在物件層級的定義（第 1 節 (1) 待決定 1）。
2. 物件鍵加上 API group（待決定 2）。會改變現有 `events_obj` 的查法，舊資料重新計算即可。
3. 時間列的粒度：建議每 10 分鐘，設定可改但只在安裝時決定。
4. `u-d67owytc34` 這類 Rancher 系統使用者改成「Rancher 內部」之後，`is_human` 相關規則（`off_hours`、`bypassed_rancher`）的對象也跟著變。要不要接受「警示數會變」，並在升級說明寫清楚。
5. 10 倍基準用 10/18（72,244 筆）還是 10/20（66,541 筆）。10/18 有中斷、又有 lab，但它是實測的最大值。
