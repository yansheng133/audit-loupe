# `/api/changemap` v0（原型）約定

寫作：2026-10-21。給平行開發用：後端（軌 B）照這個回應，介面（軌 C）照這個讀。
v0 是原型：開頁時直接查現有資料表，**不做彙總表、不調效能**。第 1 層的正式版（`docs/layer1-plan.md`）之後再換，欄位盡量沿用。

> 0.7.0 起，介面的變更地圖（v6 四個分頁）改讀 `/api/behaviors`（約定見 `docs/api/behaviors-v0.md`）。這支 API 照舊提供、欄位意義不變，物件明細仍用 `/api/changemap/detail`。

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
| `system` | `1` 時包含系統元件與 Rancher 介面維護動作；預設不含。Rancher 內部的帳號（Fleet、NeuVector、cluster agent）改到應用程式 namespace 的物件，預設就列出；改到它們自己的 namespace（`kube-*`、`cattle-*`、`fleet-*`、`*-system`）或叢集範圍物件的才隱藏 |

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

原有的參數、欄位與 cursor 意義都不變；讀索引和讀整天的結果逐欄位相同（`backend/src/shown.rs` 與 `perf.rs` 的測試會比對）。新增的欄位、參數與行為如下：

1. **`meta.progress`**：有會改變畫面內容的工作正在進行時，回應帶 `meta.progress`，每一項是 `{"what": "index" | "recompute", "startedMs": <開始時間>, "done": <已完成或 null>, "total": <總量或 null>}`。
   - `index`：升級後第一次啟動，正在建立開頁用的索引。這段期間開頁照舊可用，只是比較慢；`done`、`total` 為 null（SQLite 不回報建索引的進度）。
   - `recompute`：規則或白名單改了之後的重新判斷；`done`、`total` 是筆數。
   - 只有 `progress` 沒有 `dataNote` 時，`meta` 裡就沒有 `dataNote` 與 `dataUntilMs`。介面要分別判斷，不能假設 `meta` 一定有 `dataNote`。
2. **事件分頁**：照 Sam 10/21 的決定，不另外存一張狀態轉變表，從當天的警示、找不到執行者的變化與缺口推出來。需求文件第 5 節 (5) 的「狀態轉變記成事件，直接讀」以此為準。
3. **理由改成代碼**（介面依 Rancher 的語言自己說，不再中英混雜）。原本的中文文字欄位保留，給直接讀 API 的人：
   - 物件：`stateReasonCode`：`alert-high`、`alert-medium`、`no-apiserver-audit`（**取決於設定**：這座叢集沒有收 apiserver audit log）、`found-on-reconnect`、`near-gap`；清楚的物件是空字串。
   - 身分：`who.noteCode`、`detail.ops[].noteCode`：`sa-rancher`、`sa-kube-system`、`sa-workload`、`admin-cert`、`rancher-unauthenticated`、`no-actor`、`system-component`、`rancher-provisioning`、`rancher-local-user`、`unclassified`。白名單的理由是管理者自己寫的文字，`noteCode` 為 null，`note` 照原文顯示。
   - `detail.dependsOn[].code`：`suse-security`。
   - 缺口：`timeline.gaps[]` 加 `collector` 與 `reasonCode: "collector-silent"`。
   - 事件：`reasonCode`：`first-alert`、`first-unattributed`、`collector-stopped`、`collector-resumed`（後兩者帶 `collector`）。
   - 關係圖的身分節點加 `group`（多個身分合併成一類）與 `noActor`（找不到執行者）。
4. **時間範圍**：`/api/changemap` 與 `/api/changemap/detail` 接受 `from`、`to`（毫秒，當天內）。總覽、件數、稽核狀態、關係、事件與細節只算範圍內的紀錄；時間列照樣是整天，讓人看得到範圍在一天裡的位置。不帶就是整天。
5. **同時開同一個畫面**：server 算同一個畫面時，其他同時要同一個畫面的人等它算完、共用結果；算完之後才來的請求會重新算，所以重新整理一定看得到新紀錄。
6. **列表少掉的兩種列**（第一層計畫 v2 的兩個顯示細節）：
   - 只計入 `hidden.requestOnly`、不列成物件的請求，多了 Rancher 介面取得目前使用者的 `selfuser`、使用者與群組的搜尋（verb `search`）、登出（resource `logout` 或 verb `logout`）。
   - 刪除 namespace 後，namespace controller 對每一種資源各送一次 `deletecollection`（實驗室一次約 110 筆）。這些紀錄併進那個 namespace 本身的列（`kind: Namespace`），不再各自成列；列上的「誰」是刪除 namespace 的人，不是 controller。細節的 `ops[]` 對這些紀錄多一個 `cascade`，值是被清空的資源（例如 `pods`），`raw.resource` 也是它；其他紀錄的 `cascade` 是 null。namespace controller 屬於系統元件，預設畫面本來就不顯示，帶 `system` 才看得到。
7. **缺口分兩種來源**（介面規格第 5 節）：缺口表裡沒有原因的是 server 沒收到心跳，有原因的是收集元件自己回報的；「開始收集」的標記不是缺口，不再算進來。
   - `timeline.gaps[]`：心跳中斷是 `kind: "heartbeat"`、`reasonCode: "collector-silent"`；收集元件回報的是 `kind: "reported"`、`reasonCode: "collector-reported"`，多一個 `cause`：`collector-restart`、`collector-state-lost`、`file-removed`、`file-replaced`。
   - 事件：心跳中斷照舊是 `collector-stopped`、`collector-resumed`，`source: "heartbeat"`；收集元件回報的是 `reported-gap-start`、`reported-gap-end`，`source: "reported"`，帶 `collector` 與 `cause`。

## 0.6.0 補充（2026-10-31）

原有的欄位與意義都不變，新增：

1. **還沒結束的中斷**：`timeline.gaps[]` 的心跳中斷多一個 `open`。
   - `open: true`：收集元件還沒回報，或已被當成換掉的節點（見下一點）；`toMs` 是現在或截斷的時間，事件分頁不會有 `collector-resumed`。
   - `open: false`：已經結束（收集元件回報了，或 watch、rancher、neuvector 換了新 Pod）。
   - 缺口表裡已經記下的缺口（收集元件回報後才寫）沒有這個欄位。
2. **`GET /api/coverage`**：`collectors[]` 與心跳中斷的 `gaps[]` 多一個 `retired`。只有在一個收集元件超過一天沒有回報、而同一座叢集同一種收集元件還有別的執行個體在回報時才是 `true`（實際上就是換掉的節點）；這時中斷的 `toMs` 是新執行個體開始回報的時間（至少中斷後一天），變更地圖與收集狀態頁相同。那座叢集這種收集元件一個都沒在回報時永遠是 `false`。
3. **watch 收集元件的回報內容**：`collectors[].details` 裡每個資源類型的值，除了 `watching`、`not served`，多了 `forbidden`（被拒，通常是 ClusterRole 沒涵蓋）與 `failing`（其他錯誤），恢復後改回 `watching`。
