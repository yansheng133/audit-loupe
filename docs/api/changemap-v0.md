# `/api/changemap` v0（原型）約定

寫作：2026-10-21。給平行開發用：後端（軌 B）照這個回應，介面（軌 C）照這個讀。
v0 是原型：開頁時直接查現有資料表，**不做彙總表、不調效能**。第 1 層的正式版（`docs/layer1-plan.md`）之後再換，欄位盡量沿用。

## 請求

`GET /api/changemap`

| 參數 | 說明 |
|---|---|
| `day` | `YYYY-MM-DD`，依 server 時區；預設今天 |
| `cluster` | 可以帶多個；不帶就是全部 |
| `namespace`、`state` | 篩選；`state` 為 `look`／`unclear`／`blind`／`clear` |
| `tab` | `overview`（預設）／`relations`／`events`。只回傳該分頁需要的區塊 |
| `cursor` | 翻頁；每頁 100 列 |
| `select` | 物件鍵（見下）；不帶就選排序第一的物件 |
| `system` | `1` 時包含系統元件與 Rancher 介面維護動作；預設不含 |

物件鍵：`cluster/apiGroup/resource/namespace/name`。core group 的 `apiGroup` 是空字串；叢集範圍的物件 `namespace` 是空字串。要分得出 Kubernetes 的 Node 與 Longhorn 的 `nodes.longhorn.io`。

## 回應

```json
{
  "day": "2026-10-18",
  "tz": "Asia/Taipei",
  "asOf": {"<cluster>": "<RFC3339，該叢集最後一筆紀錄>"},
  "counts": {"objects": 0, "ops": 0, "look": 0, "unclear": 0, "blind": 0, "clear": 0},
  "hidden": {"system": 0, "uiMaintenance": 0},
  "timeline": {
    "slotMinutes": 10,
    "lines": [{"cluster": null, "slots": [{"t": 0, "state": "clear", "ops": 0, "alerts": 0}]}],
    "gaps": [{"cluster": "", "fromMs": 0, "toMs": 0, "kind": "heartbeat", "reason": ""}]
  },
  "rows": [{
    "key": "<物件鍵>", "kind": "Pod", "name": "", "namespace": "", "cluster": "",
    "state": "look", "severity": "high", "stateReason": "",
    "ops": 0, "lastMs": 0,
    "ticks": [{"t": 0, "alert": false}],
    "who": {"name": "", "category": "human", "note": ""},
    "via": "rancher"
  }],
  "next": null,
  "relations": {"nodes": [], "edges": []},
  "events": [],
  "detail": {
    "key": "", "state": "", "severity": "", "stateReason": "",
    "strip": [{"t": 0, "alert": true}],
    "ops": [{"t": 0, "who": "", "category": "", "note": "", "verb": "", "via": "", "raw": {"verb": "", "resource": "", "user": ""}}],
    "dependsOn": [""],
    "hasVersions": true,
    "hashChain": "ok"
  }
}
```

數字只是格式示意，不是資料。

## 欄位規則

- **時間**一律 UTC 毫秒；畫面依 `tz` 顯示。
- **`state`**（四種，原型直接算）：
  - `look`：當天有中、高風險警示；`severity` 為 `high`／`medium`。
  - `unclear`：當天有找不到執行者的變更（`versions` 中 `event_id IS NULL AND change <> 'baseline'`）。`stateReason` 要寫真正的原因：該叢集沒收 apiserver audit log，或重新連線後比對。依該叢集的收集元件判斷，不能寫死。
  - `blind`：當天有紀錄落在缺口起訖前後一格（10 分鐘）內。
  - `clear`：以上都不是。
  - 判斷順序就是上面的順序。
- **`timeline.lines`**：
  - 沒帶 `cluster` 時只有一條，`cluster` 為 `null`，表示合併線；每格取最嚴重的狀態。
  - 帶了叢集就每個叢集一條。只帶一個叢集時 `slotMinutes` 為 1。
  - 格子的 `state` 另有：`none`（這格沒有操作）、`blind`（這格落在缺口內）、`unknown`（還無法判斷）。
- **`gaps.kind`**：
  - `heartbeat`：server 判定收集元件沒有回報。
  - `reported`：收集元件主動回報。
  - 兩種都列，不合併。v0 若後端還沒有 `reported`（PR #11 未合併），只回 `heartbeat`。
- **`who.category`**：`human`／`component`／`serviceaccount`／`rancher`／`unknown`。
  - 依 `docs/lab/report-ui-facts.md` 第 B 節的建議分類與依據實作。
  - 白名單在 v0 先用設定檔或常數，正式版再進規則設定。
  - `note` 是一句白話說明，例如「本機帳號，可能多人共用」「憑證身分，對不到個人」。
- **`rows` 排序**：依 state（look → unclear → blind → clear），再依 `lastMs` 新到舊，再依物件鍵。`next` 是下一頁的 cursor，沒有下一頁時為 `null`。
- **`relations`**：
  - `nodes`：`{id, type: identity|via|cluster|object, label, state?, category?}`。
  - `edges`：`{from, to, count, bypass}`。`bypass` 為 true 表示直連 apiserver、繞過 Rancher。
  - 身分超過 20 個時，依 category 合併成群組節點。
- **`events`**：
  - 每筆 `{t, subject, from, to, reason, source}`。`source` 為 `state`（物件狀態轉變）、`heartbeat` 或 `reported`（收集）。
  - v0 的物件狀態轉變可以用「第一次出現警示」「第一次出現執行者不明」來近似；`reason` 要寫明是近似。
- **`detail.hasVersions`**：這類資源有沒有收物件版本。沒有時，介面停用三個動作按鈕。
- **`detail.hashChain`**：`ok`／`broken`／`pending`，取自既有的 integrity 結果。

## v0.1 變更（2026-10-21，Sam 同意）

以下五點取代上面對應的規則；其他不變。

1. **整批動作與沒有名稱的寫入要出現**（v0 會跳過 `name` 為空的紀錄）：
   - `deletecollection`：當成一列，物件鍵為 `cluster/apiGroup/resource/namespace/*`；`name` 為 `*`，`kind` 照資源種類，`batch: true`。
   - 沒有名稱的 `create`：先依資源分類，再照下面處理：
     - 會留下物件的資源（例如用 `generateName` 建立）：能從紀錄取得實際名稱就用實際名稱；取不到就放進同一個 `*` 列，`batch: true`。
     - 只是請求、不會留下物件的資源（例如 `tokenreviews`、`subjectaccessreviews`、`selfsubjectaccessreviews`、`selfsubjectreviews`、`localsubjectaccessreviews`）：不算物件變更，不列；計入 `hidden.requestOnly`。
     - 分不出來的：列出來，`batch: true`，不要藏。
2. **`cursor` 改成不透明字串**：由 server 依（狀態順序、`lastMs`、物件鍵）編碼，用 keyset 方式翻頁，不再用位移。介面只能原樣帶回，不能解析。
3. **`detail.dependsOn` 只放設定項目**：
   - 格式改為 `[{"setting": "是否啟用 SUSE Security 收集", "text": "容器裡輸入的指令不在 audit log。……"}]`。沒有就是空陣列。
   - 相關物件改放新欄位 `detail.related`：`[{"rel": "mounts", "kind": "ConfigMap", "namespace": "", "name": ""}]`。
4. **細節單獨一支 API**：`GET /api/changemap/detail?day=&key=`，只回 `detail` 物件。點一列時介面只打這支，不重打 `/api/changemap`。`/api/changemap` 開頁時照舊附預設選取的 `detail`。
5. **原型標示**：server 設定了環境變數 `AL_DATA_NOTE` 時，回應多一個 `meta`：`{"dataNote": "<原文>", "dataUntilMs": <資料最後一筆的時間>}`。介面看到 `meta` 就在分頁列下方顯示一行提示。

## v0.1 補充（2026-10-21，依軌 1、軌 3 的回報）

1. **進入容器**：子資源是 `exec`、`attach`、`portforward` 時，不論 verb 是 `get`（新版 kubectl 用 websocket 開啟）還是 `create`（舊版），動作一律顯示成子資源名稱，`dependsOn` 也照這個判斷。其他子資源維持原規則：只有 `create` 才用子資源名稱。
2. **`/api/changemap/detail` 也接受 `system`**：意思和 `/api/changemap` 的 `system` 相同。
3. **`/api/changemap/detail` 的 404**：
   - 物件當天沒有紀錄時回 404，內容是 `{"error": "no-records"}`。
   - 沒有這個 JSON 內容的 404，代表後端沒有這支 API（舊版）。介面這時才改打整頁。

## v0.2（2026-10-21 晚）：開頁速度與進度

欄位、參數與 cursor 都不變，回應內容與 v0.1 逐欄位相同（`backend/src/shown.rs` 與 `perf.rs` 的測試會比對）。只多了下面兩點：

1. **`meta.progress`**：有會改變畫面內容的工作正在進行時，回應帶 `meta.progress`，每一項是 `{"what": "index" | "recompute", "startedMs": <開始時間>, "done": <已完成或 null>, "total": <總量或 null>}`。
   - `index`：升級後第一次啟動，正在建立開頁用的索引。這段期間開頁照舊可用，只是比較慢；`done`、`total` 為 null（SQLite 不回報建索引的進度）。
   - `recompute`：規則或白名單改了之後的重新判斷；`done`、`total` 是筆數。
   - 只有 `progress` 沒有 `dataNote` 時，`meta` 裡就沒有 `dataNote` 與 `dataUntilMs`。介面要分別判斷，不能假設 `meta` 一定有 `dataNote`。
2. **事件分頁**：照 Sam 10/21 的決定，不另外存一張狀態轉變表，從當天的警示、找不到執行者的變化與缺口推出來。需求文件第 5 節 (5) 的「狀態轉變記成事件，直接讀」以此為準。
