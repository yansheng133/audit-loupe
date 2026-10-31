# `/api/behaviors` v0 約定（變更地圖 v6：行為段落）

寫作：2026-11-01（軌 1）。給平行開發用：後端照這個回應，介面（軌 2）照這個讀。
任務書：`docs/lab/task-1101-behaviors.md` 第 4、5 節。

既有的 `/api/changemap`、`/api/changemap/detail` 不變（`docs/api/changemap-v0.md`）；這是**新增**的 API。
v6 的四個分頁（活動總覽、稽核發現、依身分、依資源範圍）都讀這一支，**開頁只送一個請求**。點一段行為看操作紀錄時才打 `/api/behaviors/detail`（和 changemap v0.1 第 4 點相同的做法）。

## 請求

`GET /api/behaviors`

| 參數 | 說明 |
|---|---|
| `day` | `YYYY-MM-DD`，依 server 時區；預設今天 |
| `cluster` | 可以帶多個（重複或逗號分隔）；不帶就是全部 |
| `system` | `1` 時也列出系統元件的段落（`tier: "system"`）；預設不列，只計入 `hidden.system` |
| `select` | 預設選取的段落 `id`；不帶就選排序第一的段落（見「排序」）。回應的 `detail` 是它的明細 |

`GET /api/behaviors/detail?day=&id=`（`system` 意思相同）：只回一段行為的 `detail` 物件。
- 這段當天沒有紀錄（例如 id 是別天的、或重建後不存在）回 404，內容 `{"error": "no-records"}`。
- 沒有這個 JSON 內容的 404 代表後端沒有這支 API（舊版）。

## 回應

```json
{
  "day": "2026-10-30",
  "tz": "Asia/Taipei",
  "dayStartMs": 0, "dayEndMs": 0,
  "settings": {"cellMinutes": 10, "joinMinutes": 60, "dropFailed": true,
               "look": ["high"], "notable": ["medium"], "tagOnly": ["bypassed_rancher", "off_hours"]},
  "summary": {
    "behaviors": 0,
    "byTier": {"look": 0, "notable": 0, "normal": 0, "unrecorded": 0, "unauthenticated": 0, "routine": 0},
    "look": [{"id": "b12", "title": {}}],
    "longGaps": [{"cluster": "", "fromMs": 0, "toMs": 0, "collectors": ["watch", "file"], "open": false}]
  },
  "hidden": {"system": 0, "uiMaintenance": 0, "requestOnly": 0, "failed": 0, "failedSystem": 0},
  "gaps": [{"cluster": "", "collector": "", "fromMs": 0, "toMs": 0, "kind": "heartbeat", "reasonCode": "collector-silent", "open": false}],
  "behaviors": [{
    "id": "b12",
    "cluster": "c-m-26kbxk5f",
    "actor": {"key": "system:admin", "name": "system:admin", "category": "human", "group": "people",
              "noteCode": "admin-cert", "note": "管理者憑證，對不到個人", "dependsOn": null},
    "scope": "nv-demo-scope-check",
    "tier": "look",
    "title": {"verb": "delete", "kind": "Namespace", "apiGroup": "", "resource": "namespaces",
              "namespace": "", "name": "nv-demo-scope-check", "t": 0, "rule": "namespace_deleted", "severity": "high"},
    "what": [{"verb": "delete", "kind": "Pod", "count": 3}],
    "fromMs": 0, "toMs": 0,
    "startedBefore": false, "continuesAfter": false,
    "segments": [{"fromMs": 0, "toMs": 0, "firstMs": 0, "lastMs": 0, "ops": 0}],
    "ops": 0,
    "objects": 0,
    "objectsTop": [{"kind": "Pod", "apiGroup": "", "resource": "pods", "namespace": "", "name": "", "ops": 0}],
    "via": ["direct"],
    "tags": ["direct", "off-hours"],
    "rules": [{"rule": "namespace_deleted", "severity": "high", "count": 1, "tagOnly": false}],
    "truncated": false
  }],
  "actors": [{"key": "", "name": "", "category": "", "group": "", "noteCode": "", "note": "", "dependsOn": null,
              "clusters": [""], "behaviors": 0, "ops": 0, "look": 0}],
  "scopes": [{"cluster": "", "scope": "", "actors": 0, "behaviors": 0, "ops": 0, "look": 0, "tier": "look"}],
  "strip": {"cellMinutes": 10, "cells": [{"t": 0, "people": 0, "routine": 0, "unrecorded": 0, "unauthenticated": 0}]},
  "overlaps": [{"actors": ["admin-key", "system:admin"], "cells": 8, "fromMs": 0, "toMs": 0}],
  "selected": "b12",
  "detail": {},
  "meta": {"progress": [{"what": "behaviors", "startedMs": 0, "done": null, "total": null}]}
}
```

數字只是格式示意，不是資料。

## 欄位規則

### 共通
- **時間**一律 UTC 毫秒；畫面依 `tz` 顯示。`dayStartMs`／`dayEndMs` 是這天在 server 時區的起訖（`[start, end)`）。
- 所有給人看的句子都是**代碼＋參數**，介面依 Rancher 的語言自己組（中、英）。後端另外給的中文 `note` 只給直接讀 API 的人。
- `settings` 是這次回應用的設定值（介面可以顯示「每格 10 分鐘」「超過 60 分鐘斷開」）。

### 一段行為怎麼切（後端判斷，四個分頁共用）
- **鍵**：同一叢集（`cluster`）、同一執行者（`actor.key`）、同一目標 namespace（`scope`）。
  - `actor.key`：有 Rancher 使用者 ID 時用 ID，否則用名稱（與 `stories`、`hourly` 相同的 `actor_key`）。同一個 Rancher 使用者改了顯示名稱仍是同一個執行者；`actor.name` 是使用者目錄裡最新的名稱，沒有就用最新一筆紀錄的名稱。
  - `scope`：一般物件是它所在的 namespace；**`Namespace` 物件本身算它自己的名稱**（刪除 namespace 算在被刪的那個 namespace）；其他叢集範圍物件是空字串（畫面顯示「叢集範圍」）。namespace controller 清空被刪 namespace 的 `deletecollection` 算在那個 namespace。
- **格子**：每 `cellMinutes`（預設 10）分鐘一格，依 server 時區的整點對齊。
- **小段（`segments`）**：同一段行為裡格子相鄰的操作是一個小段（畫實心塊）；不相鄰的小段之間畫虛線。
  - `fromMs`／`toMs`：小段第一格的開始與最後一格的結束（`toMs` 不含），畫塊用。
  - `firstMs`／`lastMs`：小段裡第一筆與最後一筆操作的實際時間。
- **斷開**（成為另一段行為）：兩筆相鄰操作相隔**超過** `joinMinutes`（預設 60）分鐘（剛好 60 分鐘不斷，60 分鐘又 1 毫秒斷），**或**兩筆之間有一段收集中斷**開始**。
  - 「之間開始」：前一筆時間 ≤ 中斷開始 < 後一筆時間。中斷開始那一刻的操作算在中斷之前。
  - 任何收集元件（file、watch、rancher、neuvector）在該叢集的中斷都算，心跳中斷與收集元件回報的中斷都算；「開始收集」的標記不算。
  - **落在中斷裡面的操作照樣算**，也照樣彼此相連（中斷只在它開始的地方切一刀）。
  - 還沒結束的心跳中斷（`gaps[].open: true`）在收集元件回報、中斷寫進缺口表之後才切開；在那之前跨過它的段落不會被切開。
- **跨午夜**：一段行為不會在午夜被切開。回應只算**這天之內**的操作（`ops`、`segments`、`what`、`objects`、`rules`、`fromMs`／`toMs` 都只算這天）；`startedBefore`／`continuesAfter` 為 true 表示同一段行為在前一天／後一天還有操作，`id` 在兩天相同。
- **所有操作都切段**，不只有警示的。哪些紀錄算「操作」與 `/api/changemap` 相同：寫入（create、update、patch、delete、deletecollection）、進入容器（exec、attach、portforward）、Rancher 的非讀取動作、有中或高風險警示的讀取；Rancher 已配對到下游紀錄的那一半不重複算。以下不算操作，只計入 `hidden`：
  - `requestOnly`：不會留下物件的請求（tokenreviews 等，與 changemap v0.1 第 1 點、v0.2 第 6 點相同）。
  - `uiMaintenance`：Rancher 介面的維護動作（與 changemap 相同；`system=1` 也不列成段落）。
  - `failed`、`failedSystem`：失敗的請求（回應碼 ≥ 400），`settings.dropFailed` 為 true（預設）時不列為操作。`failed` 是預設畫面會顯示的身分，`failedSystem` 是系統元件。帶中、高風險警示的失敗請求照樣列出（警示就是在講這個失敗）。
  - `system`：系統元件的操作（下面的 `group: "system"`）。帶 `system=1` 時列成段落，這裡仍照算。
- 沒有執行者的變化（watch 看到、但對不到 audit 紀錄的物件版本，與 changemap 的「找不到執行者」同一批）也切成段落，`actor.key` 為空字串。
- `hidden` 的五個數字互不重疊，都只算這天。

### 執行者分組（`actor.group`）與身分（`actor.category`、`actor.noteCode`）
`category`、`noteCode` 與 changemap 的 `who.category`、`who.noteCode` 相同（`docs/api/changemap-v0.md` v0.2 第 3 點），另加：

| `group` | 是誰 | 畫面 |
|---|---|---|
| `people` | `category` 為 `human`，以及看不出是人或程式的（`unknown` 且 `noteCode` 為 `unclassified`） | 人與管理帳號，展開 |
| `unrecorded` | 沒有執行者（`actor.key` 為空） | 未記錄執行者，收合 |
| `unauthenticated` | `system:cattle:error`（`noteCode: "rancher-unauthenticated"`） | Rancher 未驗證請求，收合 |
| `routine` | `serviceaccount`（工作負載的 ServiceAccount），以及在應用程式 namespace 裡的 Rancher 帳號（`category: "rancher"`） | 例行，收合、淡色 |
| `system` | `component`，以及在 Rancher 自己的 namespace（`kube-*`、`cattle-*`、`fleet-*`、`*-system`）或叢集範圍的 Rancher 帳號 | 預設不列；`system=1` 才列 |

- 「找不到執行者」這個名稱不再使用。`unrecorded` 的 `noteCode`：
  - `no-apiserver-audit`：這座叢集**沒有** file 收集元件回報（只有 watch 看到變化）。
  - `found-on-reconnect`：這座叢集有收 apiserver audit log，這筆是中斷後重新比對物件版本才發現、對不到 audit 紀錄的變化。
  - 依 `heartbeats` 裡這座叢集實際回報過的收集元件判斷，不寫死（與 changemap 的 `stateReasonCode` 同一條規則）。
- `actor.dependsOn`：只有 `no-apiserver-audit` 時有值，其他是 null：
  `{"code": "no-apiserver-audit", "cluster": "local", "collectors": ["rancher", "watch"]}`。
  介面據此顯示小標「取決於設定：<cluster> 未啟用 apiserver audit log」；`collectors` 是這座叢集實際回報的收集元件（依名稱排序）。
- `unauthenticated` 的請求照常列出（軌 0：10/30 的 272 筆有 271 筆 HTTP 200）；失敗的依 `dropFailed` 計入 `hidden.failed`。

### 分級（`tier`）
依序判斷，第一個符合的就是：
1. `look`（要看）：段內有警示，其規則**不在** `tagOnly`、而且嚴重度在 `settings.look`（預設 `high`）。
2. `notable`（值得知道）：段內有警示，其規則不在 `tagOnly`、而且嚴重度在 `settings.notable`（預設 `medium`）。
3. 其餘依 `actor.group`：`people` → `normal`（一般操作）；`unrecorded`、`unauthenticated`、`routine`、`system` 照名稱。

- `tagOnly` 的規則（預設 `bypassed_rancher`、`off_hours`）**不影響分級**，只出現在 `tags`、`rules[].tagOnly: true`。
- 警示只有人的操作才會有（規則只看人），所以 `routine`、`unrecorded`、`unauthenticated` 實際上不會變成 `look`／`notable`。
- 三個設定值可以在規則設定調整（`AL_BEHAVIOR_LOOK`、`AL_BEHAVIOR_NOTABLE`、`AL_BEHAVIOR_TAG_ONLY`），改了不必重建，下一次開頁就生效。

### 一段行為的欄位
- **`id`**：字串，原樣帶回，不要解析。有執行者的段落是 `b<數字>`；沒有執行者的段落是 `u~<cluster>~<scope>~<第一筆時間>`。規則重新判斷或切段設定改變後重建，`id` 會換；新紀錄把兩段接起來時，合併後留較小的那個 `id`。
- **`title`**：段內**最嚴重的那一筆**（最高嚴重度、不在 `tagOnly` 的警示；同級取最早的一筆）。沒有這種警示時是 `null`，介面改用 `scope`＋`what` 當標題（例如「nv-demo：修改 Deployment ×12」）。
  - `verb` 是**實際的動作**：進入容器時是子資源名稱（`exec`、`attach`、`portforward`），不是規則名稱；其他子資源只有 `create` 才用子資源名稱（與 changemap 的 `detail.ops[].verb` 相同）。沒有執行者的變化是 `added`、`modified`、`deleting`、`deleted`、`added_unseen`、`modified_unseen`。
  - `kind` 依資源種類（`resource_kinds` 表，沒有就用內建對照，再沒有就是資源名稱）。
- **`what`**：「動作 種類 ×次數」，依次數多到少、全部列出；`verb` 規則同 `title`。介面自己決定顯示前幾項。
- **`ops`**：這天的操作筆數（全量，不截斷）。**`objects`**：動到的物件數。**`objectsTop`**：依筆數多到少的前 20 個；`objects` > 20 時 `truncated` 為 true 的意思**不是**這個（見下）。
- **`via`**：段內出現過的管道（`direct`、`rancher`、`impersonation`、`ace`、`unknown`），依名稱排序。
- **`tags`**：段落上的標籤代碼：`direct`（有直連 apiserver 的操作，`routine` 不標）、`via-rancher`（有經 Rancher 的操作）、`off-hours`（有 `off_hours` 警示）、`bypassed-rancher`（有 `bypassed_rancher` 警示）。
- **`rules`**：段內的警示，每條規則一列，`count` 是筆數，依嚴重度（高、中、低）再依規則名稱排序。
- **`truncated`**：`/api/behaviors/detail` 的操作紀錄會不會被截斷（`ops` > 500）。所有計數本身都是全量。

### 排序
`behaviors` 依 `tier`（look → notable → normal → unrecorded → unauthenticated → routine → system），再依 `fromMs` 早到晚，再依 `id`。

### 其他區塊
- **`summary`**：活動總覽上方那一句話要用的數字。
  - `behaviors`：這天的段落數（不含 `system`）；`byTier` 各級段數。
  - `look`：要看的段落與其 `title`，依時間。
  - `longGaps`：同一叢集的收集中斷合併後（重疊的接起來）長度 ≥ `joinMinutes` 的，`collectors` 是涵蓋的收集元件。
- **`gaps`**：這天的收集中斷，欄位與 `/api/changemap` 的 `timeline.gaps` 相同（`kind`、`reasonCode`、`cause`、`open`），原本「事件」分頁的收集事件在稽核發現的最後一區用它畫。
- **`actors`**：依身分分頁左邊的帳號清單；一個 `key` 一列（跨叢集合併，`clusters` 列出出現的叢集）。`behaviors`、`ops`、`look` 是段數、筆數、要看的段數。依 group（people、unrecorded、unauthenticated、routine、system）再依最嚴重的 tier、再依筆數多到少。
- **`scopes`**：依資源範圍分頁左邊的 namespace 清單；一個（`cluster`, `scope`）一列。`actors` 是動過它的帳號數，`tier` 是它最嚴重的段落的 tier。依叢集、再依 `tier`、再依段數多到少。
- **`strip`**：底部時間列，這天每一格各組的操作筆數（只列有操作的格子，`t` 是格子開始時間）。
- **`overlaps`**：`people` 裡兩個不同帳號在同一格都有操作的格數，以及第一格開始到最後一格結束。**只是提示，不能說是同一人**（任務書第 3 節第 6 點）。
- **`selected`**、**`detail`**：預設選取的段落與其明細（同 `/api/behaviors/detail`）；這天沒有段落時兩者都是 null。
- **`meta`**：
  - `meta.progress`：有會改變內容的工作正在進行時才有。`what: "behaviors"` 是升級後第一次啟動、或切段設定改了之後在建行為表；建完之前 `behaviors` 是空陣列、`summary.behaviors` 為 0，介面要顯示「正在整理」而不是「這天沒有操作」。`what: "recompute"` 與 changemap 相同。
  - `meta.dataNote`、`meta.dataUntilMs`：與 changemap v0.1 第 5 點相同。

### `detail`（`/api/behaviors/detail`）

```json
{
  "id": "b12",
  "ops": [{"t": 0, "verb": "delete", "kind": "Namespace", "apiGroup": "", "resource": "namespaces", "namespace": "", "name": "",
           "via": "direct", "code": 200, "severity": "high", "rules": ["namespace_deleted"], "cascade": null}],
  "opsTotal": 0,
  "truncated": false,
  "objects": [{"kind": "", "apiGroup": "", "resource": "", "namespace": "", "name": "", "ops": 0, "key": "<changemap 物件鍵>"}],
  "objectsTotal": 0
}
```
- `ops`：這天這段的操作，依時間早到晚，最多 500 筆；超過時 `truncated: true`，`opsTotal` 是全部筆數，介面要寫明「只列前 500 筆」。沒有執行者的變化 `via` 是 `unknown`、`code` 是 null。
- `objects`：全部動到的物件（最多 500 個，`objectsTotal` 是全部），`key` 可以直接拿去打 `/api/changemap/detail`。

## 設定（環境變數，Helm values `behaviors.*`）

| 環境變數 | 預設 | 說明 | 改了之後 |
|---|---|---|---|
| `AL_BEHAVIOR_CELL_MINUTES` | `10` | 格寬（分鐘），要能整除 1440 | 啟動時重建行為表 |
| `AL_BEHAVIOR_JOIN_MINUTES` | `60` | 兩筆操作相隔超過這麼久就斷開 | 啟動時重建 |
| `AL_BEHAVIOR_DROP_FAILED` | `true` | 失敗的請求（≥ 400）不算操作 | 啟動時重建 |
| `AL_BEHAVIOR_LOOK` | `high` | 算「要看」的嚴重度（逗號分隔） | 不必重建 |
| `AL_BEHAVIOR_NOTABLE` | `medium` | 算「值得知道」的嚴重度 | 不必重建 |
| `AL_BEHAVIOR_TAG_ONLY` | `bypassed_rancher,off_hours` | 只當標籤、不影響分級的規則 | 不必重建 |
