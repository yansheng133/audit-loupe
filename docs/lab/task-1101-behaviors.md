# 任務 1101：變更地圖 v6——行為段落與四種呈現方式

寫作：2026-10-31 深夜（Claude，對話端）。Sam 10/31 同意方向與分工：實作全部交給 CLI。
本任務分成三軌：**軌 0（先做、唯讀）**、**軌 1 後端**、**軌 2 介面**。軌 1、軌 2 可以各開一個 CLI、各自 worktree 平行做。

## 0. 這份任務書附帶的檔案（全部在同一個 zip 裡）

Sam 會把 zip 解開。下面的路徑是**建議放置位置**；第一步請確認檔案真的在那裡，不在就先問 Sam，不要照名稱猜內容。

| 檔案 | 建議位置 | 是什麼 |
|---|---|---|
| `task-1101-behaviors.md` | 主 repo `docs/lab/task-1101-behaviors.md` | 本文件 |
| `v6/audit-loupe-views.html` | UI repo `docs/design/v6/audit-loupe-views.html` | **設計基準**：可點擊模型，單一 HTML，資料已內嵌，瀏覽器直接開（字型連 Google Fonts，離線會退回系統字型）。Sam 已看過並認可方向 |
| `v6/template.html` | 同上目錄 | 模型的原始碼（`__DATA__` 是資料插入點）；行為切段的演算法在裡面，搜尋 `// ---- behaviors` |
| `v6/prep.py` | 同上目錄 | 把匯出資料整理成模型用的 `data.json` 的腳本 |
| `v6/data.json` | **不建議放進 repo**（見第 6 節） | 10/30 lab 資料整理後的結果，軌 2 開發時可當假資料 |

10/30 的原始匯出（`changemap-rows.json`、`changemap-details.json`、`changemap-events.json`、`stories.json`）是 Sam 的 CLI 今天產生的 zip，**還在 Sam 那邊**，本 zip 沒有重附。

## 1. 現況（已確認，不用重查）

寫作時的 repo 狀態（第一步請 `git log -1` 實測，比這個新就以實測為準）：
- 主 repo `yansheng133/audit-loupe`：`2503a3f`（Merge PR #30，2026-10-31 20:51 +0800）
- UI repo `yansheng133/audit-loupe-ui`：`e7eaf2d`（Merge PR #13 release/0.6.0）

lab 的叢集：
- 下游 RKE2：叢集 ID `c-m-26kbxk5f`（模型裡顯示成 `rke2`）
- Rancher 本身：`local`

後端既有、與本任務直接相關的程式（**我讀過原始碼，沒有執行過**）：
- `backend/src/store.rs` 的 `add_to_story_in`：`stories` 表＝同一叢集、同一 `actor_key`、同一 `via`，與既有段落頭尾相距 10 分鐘內就併入。**只收有警示的操作**。`rulestate.rs` 的 `recompute()` 用影子表（`rc_stories`）重建它與 `hourly`。
- `backend/src/changemap.rs`：`DETAIL_OPS = 200`、`TICKS = 200`。物件明細每個物件最多回 200 筆。
- `events` 表有 `code` 欄位（HTTP 回應碼）。**我沒有查過它的值**。
- `backend/src/rancher.rs` 的 `worth_keeping`：Rancher audit log 只留寫入、Norman action、login、Secret 讀取。
- 身分分類依 `docs/lab/report-ui-facts.md` 第 B 節；`system:cattle:error` 的 group 是 `unauthenticated`，lab 曾實測：未登入打 Rancher API 回 401 時會記成這個身分（`docs/lab/report-rotation-live.md`）。

10/30 匯出的事實（`/api/changemap?day=2026-10-30` 預設畫面）：
- 207 個物件、7,500 筆操作；`hidden.system` 7,352。
- 收集中斷（心跳）：rke2 `10:06–18:53`（watch、file），local `10:07–18:53`（rancher、watch），另有幾段數分鐘的短中斷（00:34、06:32、22:42 前後）。
- 警示：`bypassed_rancher`（中）124 次、`pod_exec`（中）67 次、`off_hours`（低）81 次、`namespace_deleted`（高）1 次，全部是 `system:admin`（直連）。
- 無執行者的紀錄全部在 `local`：
  - 執行者空白 164 筆，`stateReasonCode = no-apiserver-audit`（local 沒有收 apiserver audit log），動作是 watch 看到的 `added／modified／deleted／deleting`。
  - `system:cattle:error` 248 筆，動作 `update`／`patch`，對象是 `fleet-default` 的 machine-plan Secret、`Cluster rke2`、Fleet bundledeployments。
- 匯出時有 7 個物件的明細被截在 200 筆（Longhorn、SUSE Observability agent、Fleet machine-plan Secret、Argo CD Application `nv-cicd-demo` 等），所以模型裡這些的筆數是下限。

## 2. 已經是好的，不要碰

- 既有的 `/api/changemap`、`/api/changemap/detail` 的欄位與意義（`docs/api/changemap-v0.md`）。只能**新增**欄位或新增 API，不能改既有欄位的意思。
- 開頁 5 秒內出資料的要求，以及「開頁只送一個請求」（v5 規格第 10 節）。
- 雜湊鏈、收集缺口的判斷、`stories` 表現有的行為（「要看」清單還在用）。
- 舊頁面：權限覆核、收集狀態、規則。
- 「收集中斷的時段不回填」原則（`docs/change-map-requirements.md`）。

## 3. 我的假設可能錯在哪（請先驗證，不要跟著錯）

1. **模型的行為切段是用截斷的明細算的。** 我用每個物件最多 200 筆的明細，在瀏覽器裡切段。後端要用 `events` 全量重算，數字會不同。第 7 節的預期值只能當參考：差異要逐項說明原因，不能說「大致相同」。
2. **`system:cattle:error` 的寫入到底有沒有成功，我不知道。** 我只看到 verb 是 update／patch，沒看到回應碼。如果全部是 4xx，它們就不是變更，不該出現在變更地圖上（Sam 已同意：失敗的請求不列入）。這是軌 0 要回答的問題。
3. **我把 `pod_exec` 寫成了「進入 Pod」。** 10/30 那 67 次其實全部是 `portforward`。規則名稱涵蓋 exec、attach、portforward 三種，所以**段落摘要要用實際的子資源名稱產生，不能用規則名稱**。
4. **`actor_key` 跟 `actor` 的差別我沒查。** 模型用的是明細 API 的 `who`。行為要用哪一個當執行者鍵，由軌 1 決定，並在報告裡寫明理由。
5. **收集中斷內也有操作紀錄。** 心跳中斷期間 rke2 06:32–06:37 仍有 system:admin 的操作進來。模型的規則是「兩筆操作之間如果有一段中斷**開始**，就斷開」，不是「落在中斷內就斷開」。我第一版寫錯了，把每一筆都切成獨立一段。這條規則請軌 1 想清楚再定，並寫成測試。
6. **「人與管理帳號同時操作」的偵測**（admin 與 system:admin 06:30–09:20 有 8 格重疊）是模型裡用 10 分鐘格子交集算的，只是提示，不能宣稱是同一人。

## 4. 目標（使用者看得到的行為）

打開變更地圖，上方有四個分頁，取代 v5 的「總覽／關係／事件」：

| 分頁 | 副標 | 英文 |
|---|---|---|
| 活動總覽 | 全日變更時間軸 | Activity |
| 稽核發現 | 依風險分級的行為 | Findings |
| 依身分 | 單一帳號的操作軌跡 | Identities |
| 依資源範圍 | 單一 namespace 的變更來源 | Scopes |

畫面以模型 `v6/audit-loupe-views.html` 為準，以下是規則：

**行為（後端判斷，四個分頁共用）**
- 一段行為＝同一叢集、同一執行者、同一「目標 namespace」的操作。目標 namespace：一般物件是它所在的 namespace；**`Namespace` 物件本身算在它自己的名稱**（刪除 namespace 算在被刪的那個 namespace）；其他叢集範圍物件算「叢集範圍」。
- 每 10 分鐘一格（依 server 時區對齊）。相鄰的格子是同一小段，畫成實心塊。
- 兩筆操作相隔 **60 分鐘以內**、但格子不相鄰：同一段行為，小段之間畫**虛線**。
- 相隔超過 60 分鐘，或中間有收集中斷開始：斷開，成為另一段行為。
- **所有操作**都要切段，不只有警示的。

**分級（Sam 同意的暫定規則，要做成可以在規則設定裡調整的值）**
- 要看：段內有高風險警示。
- 值得知道：段內有中風險警示，而且不是 `bypassed_rancher`。
- `bypassed_rancher`、`off_hours`：只當段落的標籤（「直連 apiserver」「非上班時間」），不影響分級。
- 一般操作：人與管理帳號的其他段落。
- 例行：工作負載的 ServiceAccount，預設收合，用淡色。
- 每段的標題是**段內最嚴重的那一筆**（例如「刪除 Namespace nv-demo-scope-check」），其餘用「動作 種類 ×次數」摘要。

**無執行者的拆分**
- 「找不到執行者」這個名稱不再使用，拆成兩類，都用中性色，不能用警示色：
  - **未記錄執行者**：該叢集沒有收 apiserver audit log，只靠 watch 看到變化。要加小標「取決於設定：<叢集> 未啟用 apiserver audit log」，原因依實際收集元件判斷，不能寫死。
  - **Rancher 未驗證請求**：`system:cattle:error`。依軌 0 的結果：失敗的請求（4xx）不列為變更，計入 `hidden` 的新欄位；成功的照常列出。

**畫面**
- 活動總覽：執行者為大列、目標 namespace 為子列的泳道，橫軸 00–24。分組：人與管理帳號（展開）、未記錄執行者、Rancher 未驗證請求、例行（後三者預設收合成一條合併線）。收集中斷畫成斜線。只有「要看」的段落用紅色並在旁邊標出標題那一筆。上方一句話摘要：「這天有 N 段行為。X 段要看：…。其餘是一般操作或例行背景。」後面接各級件數的標籤，以及長時間收集中斷的標籤。
- 稽核發現：依分級分區列出段落；一般操作、無執行者、例行預設收合；最後一區是收集中斷（原本 v5「事件」分頁的收集事件移到這裡）。
- 依身分：左邊帳號清單，右邊是那個帳號依 namespace 分列的泳道，加上段落清單。
- 依資源範圍：左邊 namespace 清單（依叢集分組），右邊是動過它的帳號泳道，加上每個帳號一行摘要。
- 右側行為明細：標題、執行者與身分說明、時間、地方、規模、管道、觸發的規則（標出哪些只當標籤）、小段列表、動到的物件、操作紀錄。資料被截斷時要寫明。
- 底部時間列：沿用 v5，選中段落時標出它的範圍。
- 中英兩套文字、亮暗主題都要。

## 5. 建議做法與分工

### 軌 0：查 `system:cattle:error` 的回應碼（唯讀，先做，約 15 分鐘）

1. 對 lab 的 Audit Loupe 資料庫（或原始 Rancher audit log）查 10/30（Asia/Taipei）`actor = 'system:cattle:error'` 的紀錄，依 `code`、`verb`、`resource` 分組計數。
2. 如果 `code` 沒有存或都是 0，改查原始 Rancher audit log 的 `responseCode`。
3. 結果寫成 `results/T-0_cattle-error.md`，並放一個旗標檔 `handoff/0-done`，內容一行：成功筆數、失敗筆數。軌 1 依此決定要不要過濾。

做不到（例如資料庫在 Pod 裡沒有 sqlite3）：A. 用後端既有 API 能查到的部分；B. 把查詢寫成腳本，請 Sam 用 `! bash <腳本>` 自己跑；C. 都不行就回報卡在哪，**不要為了這一步卡住軌 1**，軌 1 先把過濾做成設定值，預設不過濾。

### 軌 1：後端（主 repo）

1. **先寫 API 約定** `docs/api/behaviors-v0.md`（形式比照 `changemap-v0.md`），寫完放旗標 `handoff/1-contract-ready`，軌 2 依此接 API。建議：新增 `tab=activity|findings|identities|scopes`，或新增 `/api/behaviors`。由你決定，但要維持「開頁一個請求」。每段行為至少要有：`id, cluster, actor{name,category,noteCode}, scope(目標 namespace 或空字串), tier, title(依代碼組成，介面自己翻譯), segments[{fromMs,toMs,ops}], ops, objects, via 標籤, rules[{rule,severity,count,tagOnly}], truncated?`。
2. 行為表在**寫入時維護**，重新判斷時跟 `stories`、`hourly` 一起重建（做法參考 `add_to_story_in` 與 `rulestate.rs`）。不能每次開頁才從 `events` 全量計算。
3. 分級對照、60 分鐘門檻、10 分鐘格寬做成設定值（預設照第 4 節）。
4. 身分分類拆出「未記錄執行者」「Rancher 未驗證請求」兩個 `noteCode`／類別；失敗請求的過濾依軌 0 結果，做成設定值。
5. 測試：至少涵蓋第 3 節第 5 點的中斷規則、60 分鐘邊界（剛好 60 分鐘、60 分鐘又 1 毫秒）、跨午夜、Namespace 物件歸屬、只有 `bypassed_rancher` 的段落不能是「要看」。

### 軌 2：介面（UI repo）

1. 等 `handoff/1-contract-ready` 之前，先用 `v6/data.json`（或依模型 `template.html` 的切段邏輯）做假資料，把四個分頁、明細、時間列做出來。
2. 約定好了就換成真 API；不要在瀏覽器裡切段（模型這樣做只是因為沒有後端）。
3. 版面、分組、色彩分工照模型；顏色改用 Rancher 主題變數（沿用 v5 的做法）。
4. 沿用 v5 的網址還原、全螢幕、側邊選單收合。

**最後手段**：某個視圖卡住超過一小時，就先照模型的靜態行為交付，並在報告裡列為未完成；不要為了一個細節卡住整個分頁。

## 6. 硬性限制

- 不可把 token、密碼寫進檔案或印到終端機；憑證只從既有檔案讀。
- 每一條 `kubectl`／`helm` 都要明確指定 kubeconfig。
- 不改第 2 節列出的東西；既有 API 欄位只能新增。
- 不改 lab 的 Rancher、RKE2、audit policy 設定（包括「在 local 開 apiserver audit log」，那是 Sam 要另外決定的事）。
- **資料不可編造**：畫面上的任何數字、時間、名稱都要來自 API。模型裡沒有的值，用占位並標「待確認」，不能用猜的。
- `v6/data.json` 與 10/30 原始匯出含 lab 的物件名稱與帳號名稱。repo 是公開的，**放不放進 repo 由 Sam 決定，先不要 commit**。
- 平行時：每軌只寫自己的檔案（`results/T-1_*`、`results/T-2_*`、`handoff/1-*`、`handoff/2-*`），共用狀態檔只改自己那一行、改之前重讀。

## 7. 驗證（用 lab 10/30 的資料）

**先套用 `checks-that-cannot-fail` 複查這一節的每一條。**

### 7.1 逐段比對（不接受「大致相同」）

模型裡 system:admin 的 8 段如下（時間為 Asia/Taipei，筆數來自截斷的明細）。後端結果逐列填「相同／不同＋原因」：

| 目標 namespace | 時間 | 小段數 | 筆數 | 分級 |
|---|---|---|---|---|
| argocd | 00:08 | 1 | 1 | 一般操作 |
| argocd | 06:36–07:26 | 2 | 31 | 值得知道 |
| argocd | 08:46–09:11 | 2 | 4 | 一般操作 |
| nv-demo | 00:38 | 1 | 1 | 值得知道 |
| nv-demo | 08:05–08:29 | 1 | 21 | 一般操作 |
| cattle-neuvector-system | 06:36–07:48 | 2 | 58 | 值得知道 |
| nv-demo-scope-check | 08:05–08:47 | 2 | 15 | **要看**（標題：刪除 Namespace nv-demo-scope-check） |
| 叢集範圍 | 08:29 | 1 | 2 | 一般操作 |

模型全部段數：63（要看 1、值得知道 3、一般操作 6、未記錄執行者 18、Rancher 未驗證請求 5、例行 30）。後端的總數會因為全量資料而不同，**列出每一類的差異與原因**。

### 7.2 反向檢查
- 只有 `bypassed_rancher`／`off_hours` 的段落，沒有一段是「要看」或「值得知道」。
- 例行與無執行者的段落，沒有一段用警示色。
- 沒有任何一段跨過 10:06–18:53 的收集中斷。
- 沒有任何兩個相鄰小段相隔超過 60 分鐘。
- 畫面上沒有「找不到執行者」字樣，也沒有「待確認」以外的編造值。

### 7.3 回歸
- `/api/changemap` 既有欄位：用同一天、同一參數，新舊版逐欄位比對（`perf.rs`／`shown.rs` 既有的比對測試照跑）。
- 開頁 5 秒內、只有一個請求（瀏覽器網路紀錄截圖）。
- `stories` 與「要看」清單的內容不變。
- 舊頁面（權限覆核、收集狀態、規則）能正常開。

### 7.4 介面
- 四個分頁 × 亮／暗 × 中／英，逐格截圖（12 格），放 `results/T-2_shots/`。
- 點「要看」那段：右側明細、底部時間列的範圍標示、活動總覽的紅色標示三者一致。

## 8. 回報格式

每軌一份 `results/T-<軌>_report.md`：
1. 診斷：第 3 節哪些假設是錯的。
2. 採用的做法（含 API 約定的最終版連結）。
3. 第 7 節的逐格結果（表格，不接受「都通過」）。
4. 任何做了、但本任務書沒提到的修改。
5. 沒做完的部分卡在哪。
6. **本任務書哪裡害你走冤枉路。**

報告裡不准出現密碼或 token。完成後各自開 PR，PR 說明附上報告。

## 結果（2026-11-01 補記）

- 軌 0：`system:cattle:error` 10/30 共 272 筆，271 筆 HTTP 200、1 筆 500，沒有 4xx；照常列為「Rancher 未驗證請求」。
- 軌 1：後端 PR #31，報告 `docs/lab/report-task-1101-backend.md`，API 約定 `docs/api/behaviors-v0.md`。
- 軌 2：介面 audit-loupe-ui PR #14，報告在該 repo 的 `docs/lab/report-task-1101-ui.md`。
- 兩邊於 11-01 合併，發佈為 0.7.0（後端 tag `v0.7.0`、介面 release `audit-loupe-0.7.0`），lab 兩座叢集與 Rancher 擴充功能已升級。
- 待 Sam 決定：既有 `/api/changemap` 要不要也不列失敗請求；設計模型 `audit-loupe-views.html`（內嵌 lab 資料）要不要進 repo；cattle 建立 Namespace `local` 回 409 的 8 筆要算系統元件還是例行。
