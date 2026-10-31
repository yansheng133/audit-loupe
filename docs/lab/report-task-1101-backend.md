# T-1：變更地圖 v6 後端——行為段落（任務 1101 軌 1）

寫作：2026-11-01（軌 1，Claude）。分支 `feat/v6-behaviors`（以 `fix/ui-usability` 為底）。所有數字都來自這次實際執行的指令；lab 資料是 `release-0.4.0/db-1031` 的副本（10/31 08:20 取出），只在本機副本上讀寫，原檔沒有動。

**一句話結論**：新增 `GET /api/behaviors`（開頁一個請求）與 `/api/behaviors/detail`；行為在寫入時切好、存在 `behaviors`／`behavior_ops` 兩張表，開頁 14 ms（lab 10/30）、71 萬筆／天的效能閘門 p95 47 ms。模型裡 system:admin 的 8 段有 7 段完全相同，1 段不同（9 筆 HTTP 400 被過濾）；全部段數 63 → 70，差異逐類說明在第 3 節，都是「模型用截斷的明細」或「失敗請求不列入」兩個原因。既有 `/api/changemap` 新舊版逐欄位比對 0 差異（10/31 的 4 個差異是「到現在為止」的中斷終點，見 7.3）。

名詞：
- **行為（段落）**：同一叢集、同一執行者、同一目標 namespace 的操作，相隔不超過 60 分鐘、中間沒有收集中斷開始。
- **小段**：一段行為裡 10 分鐘格子相鄰的操作（畫實心塊）；小段之間畫虛線。
- **目標 namespace（scope）**：物件所在的 namespace；Namespace 物件算它自己的名稱；其他叢集範圍物件是空字串（「叢集範圍」）。
- **失敗請求**：回應碼 ≥ 400。

## 1. 診斷：任務書第 3 節哪些假設是錯的

| # | 假設 | 結果 | 依據 |
|---|---|---|---|
| 1 | 模型用截斷的明細切段，全量會不同 | **對**。7 個被截在 200 筆的物件造成多數差異（第 3 節） | 7.1 逐列比對 |
| 2 | `system:cattle:error` 可能全是 4xx | **錯**（軌 0）：10/30 272 筆中 271 筆 HTTP 200、1 筆 500 | `results/T-0_cattle-error.md`；後端把那 1 筆 500 計入 `hidden.failed`，fleet-default 23:01 那段因此變成 23:16 開始 |
| 3 | `pod_exec` 要用實際子資源命名 | **對**。10/30 `pod_exec` 67 次全是 `portforward`（create 27、get 40）；三段「值得知道」的標題 `verb` 都是 `portforward` | DB 查詢；API `title.verb` |
| 4 | `actor_key` 與 `actor` 的差別沒查 | 有差：Rancher 使用者的 `actor_key` 是使用者 ID（`user-kgw7z`），`actor` 是紀錄裡的名稱（`admin`）。**選 `actor_key`**，理由見第 2 節 | 回應 `actors[]` |
| 5 | 中斷規則是「中斷**開始**在兩筆之間才斷」 | 採用，並定清楚邊界：前一筆 ≤ 中斷開始 < 後一筆 才斷；中斷內的操作照算、彼此照樣相連 | 測試 `a_gap_cuts_where_it_starts_and_records_inside_it_still_count` |
| 6 | 人與管理帳號同時操作只是提示 | 照做成 `overlaps[]`，只給格數與起訖，不判斷是否同一人。10/30：`system:admin` 與 `user-kgw7z`（admin）8 格、06:30–09:20，與模型相同 | 回應 `overlaps` |

另外發現：任務書說 `off_hours` 81 次，後端段落裡是 201 次。81 是 stories（只收中、高風險）裡附帶的；後端算的是所有列出操作上的 `off_hours`（低風險也算）。這不影響分級（`off_hours` 只當標籤）。

## 2. 採用的做法

API 約定最終版：`docs/api/behaviors-v0.md`（commit `0dc3d0c` 初版；`96b6e0f` 補四點；v0.1 補充回答軌 2 的六個問題，變更紀錄在 `handoff/1-contract-changes`）。

| 項目 | 做法 | 理由 |
|---|---|---|
| API 形式 | 新增 `GET /api/behaviors?day=&cluster=&system=&select=`，一次回四個分頁要的全部（段落、分級件數、帳號清單、namespace 清單、時間列、重疊提示、缺口、預設選取段落的明細）；點段落才打 `/api/behaviors/detail?day=&id=` | 不動 `/api/changemap` 的意義；維持開頁一個請求 |
| 寫入時維護 | `behavior_ops`（每筆可列的紀錄一列：所屬段落，或只計數的種類 request／ui／failed）＋`behaviors`（鍵、首末時間、筆數）。新紀錄找同鍵、前後 60 分鐘內、中間沒有中斷開始的段落併入；同時接上兩段就合併（留較小的 id） | 與 `add_to_story_in` 同樣在 ingest 的 transaction 裡 |
| 中斷晚寫入 | 心跳中斷要等收集元件回報才寫進缺口表；寫入時（`heartbeat`、`record_gap`、`prune`）把跨過中斷開始的段落切開（`sync_gaps`，記住處理到哪一筆缺口） | 段落先存、缺口後到時結果仍正確；測試兩種順序結果相同 |
| 重建 | 規則重新判斷時與 stories、hourly 一起在影子表（`rc_behaviors`）重建後換上；第一次啟動或切法設定改了時在背景建一次（`rb_behaviors`，分段持鎖、兩段之間讓出鎖），用 `behavior_state.fingerprint` 防止重複 | 規則會改變「哪些讀取算操作」（有中、高風險警示的讀取），所以要跟著重建 |
| 開頁時才套用的 | 身分分類（白名單改了不必重建）、分級（look／notable／tagOnly）、格寬 | 這些不改變切法 |
| 沒有執行者的變化 | watch 看到、對不到 audit 紀錄的版本沒有紀錄可掛，開頁時從 `versions` 的索引讀當天（前後各加 60 分鐘）用同一條規則切 | 一天幾百筆（10/30 為 164 筆左右），而且版本之後可能才對到紀錄，存起來反而會過期 |
| 執行者鍵 | `actor_key`（有 Rancher 使用者 ID 用 ID，否則名稱） | 與 `stories`、`hourly` 相同；同一人改顯示名稱不會斷成兩個人；顯示名稱取使用者目錄最新的（所以 admin 顯示為 `Default Admin`） |
| 中斷規則 | 前一筆 ≤ 中斷開始 < 後一筆就斷；該叢集**任何**收集元件的中斷都算（不合併），「開始收集」標記不算 | 任何一個收集元件停了，那之後的紀錄就可能缺；中斷開始那一刻的操作算之前。模型用「合併後的中斷、嚴格不等」——10/30 兩者結果相同（7.1 八列沒有因此不同） |
| 目標 namespace | Namespace 物件算自己的名稱；namespace controller 清空被刪 namespace 的 `deletecollection` 算那個 namespace；其他叢集範圍物件 `""` | 任務書第 4 節 |
| 分組 | human → 人與管理帳號；看不出是人或程式的也放這裡（不藏）；serviceaccount 與在應用程式 namespace 的 Rancher 帳號 → 例行；`system:cattle:error` → Rancher 未驗證請求；無執行者 → 未記錄執行者；component 與在 Rancher 自己 namespace 的 Rancher 帳號 → 系統（預設不列，計入 `hidden.system`） | 與 changemap 預設畫面隱藏的範圍一致（差異見第 4 節） |
| 未記錄執行者的原因 | 看 `heartbeats` 裡該叢集實際回報的收集元件：沒有 file → `no-apiserver-audit`，附 `dependsOn: {cluster, collectors}`；有 file → `found-on-reconnect` | 不寫死；與 changemap 的 `stateReasonCode` 同一條規則 |
| 失敗請求 | 預設不列，計入 `hidden.failed`（預設畫面會顯示的身分）與 `hidden.failedSystem`（系統元件）；帶中、高風險警示的失敗請求照樣列出 | 依軌 0；警示本身在講那個失敗時不能藏 |

設定（環境變數＋Helm `server.behaviors.*`＋`docs/deploy.md` 9.2）：`AL_BEHAVIOR_CELL_MINUTES=10`、`AL_BEHAVIOR_JOIN_MINUTES=60`、`AL_BEHAVIOR_DROP_FAILED=true`、`AL_BEHAVIOR_LOOK=high`、`AL_BEHAVIOR_NOTABLE=medium`、`AL_BEHAVIOR_TAG_ONLY=bypassed_rancher,off_hours`。設定錯誤時設定檢查失敗（測試 `settings_are_checked`）。`helm template` 實測：預設輸出這 6 個值，`--set server.behaviors.dropFailed=false,joinMinutes=30` 輸出 `"false"`、`"30"`。

## 3. 第 7 節逐格結果

### 7.1 逐段比對（system:admin 8 段）

| 目標 namespace | 模型 | 後端 | 結果 |
|---|---|---|---|
| argocd | 00:08・1 小段・1 筆・一般操作 | 00:08・1・1・一般操作（b781） | 相同 |
| argocd | 06:36–07:26・2・31・值得知道 | 06:36–07:26・2・31・值得知道（b830），標題 portforward Pod argocd-server-… | 相同 |
| argocd | 08:46–09:11・2・4・一般操作 | 08:46–09:11・2・4・一般操作（b934） | 相同 |
| nv-demo | 00:38・1・1・值得知道 | 00:38・1・1・值得知道（b821），標題 portforward | 相同 |
| nv-demo | 08:05–08:29・1・21・一般操作 | 08:05–08:29・**2・12**・一般操作（b927） | **不同**：21 筆中 9 筆是 `create pods` 回 HTTP 400（08:18:32 五筆、08:28:34 三筆、08:29:13 一筆），失敗請求不列入。08:18 那格只有失敗的請求，拿掉後 08:05–08:06 與 08:28–08:29 不再相鄰，變成 2 小段 |
| cattle-neuvector-system | 06:36–07:48・2・58・值得知道 | 06:36–07:48・2・58・值得知道（b829） | 相同 |
| nv-demo-scope-check | 08:05–08:47・2・15・要看（刪除 Namespace nv-demo-scope-check） | 08:05–08:47・2・15・要看（b929），標題 delete Namespace nv-demo-scope-check（namespace_deleted，high） | 相同 |
| 叢集範圍 | 08:29・1・2・一般操作 | 08:29・1・2・一般操作（b933） | 相同 |

### 7.1 全部段數（模型 63 → 後端 70）

| 分級 | 模型 | 後端 | 差異與原因 |
|---|---|---|---|
| 要看 | 1 | 1 | 相同（上表） |
| 值得知道 | 3 | 3 | 相同（上表） |
| 一般操作 | 6 | 6 | 段數相同。system:admin nv-demo 筆數不同（上表）。admin 在 rke2 cattle-neuvector-system 06:38–09:38 4 小段：模型 143 筆、後端 138 筆，少的 5 筆是失敗請求（400、401、401、405、409）；名稱顯示 `Default Admin`（使用者目錄的名稱，鍵是 `user-kgw7z`），模型顯示 `admin`。local 的 admin 那段 1 筆相同、名稱同上 |
| 未記錄執行者 | 18 | 22 | local 的 18 段逐段相同。多 4 段在 rke2 suse-observability-agent（00:31、00:35–01:04、07:26、22:45；ConfigMap `stackstatetoken`、`stackstate-leader-election` 沒對到紀錄的版本）。模型漏掉是因為這兩個物件當天 1,732、2,295 筆，匯出只留最近 200 筆。rke2 有 file 收集元件，所以原因是 `found-on-reconnect`、不是 `no-apiserver-audit` |
| Rancher 未驗證請求 | 5 | 5 | 3 段相同。fleet-default 00:03–01:05：166 → 190 筆（machine-plan Secret 被截在 200 筆，實際 224，差 24，與軌 0 相同）。fleet-default 23:01–23:46 → 23:16–23:46、4 → 3 筆（23:01:45 那筆是 HTTP 500） |
| 例行 | 30 | 33 | ci-runner 8 → 6（06:32 HTTP 429、10:07 HTTP 504 兩段只有失敗請求）；argocd-application-controller 9 → 11（Application `nv-cicd-demo` 當天 497 筆被截在 200，模型少了 00:08–00:32、00:37–00:59 兩段，06:37 開始那段模型從 08:17 開始、101 → 144 筆；nv-demo 08:32–09:33 13 筆中 12 筆 HTTP 400，剩 09:33 1 筆）；longhorn 8 → 7（01–04 時的寫入全部失敗：169、195、199、114 筆；06:32:41、06:33:28 兩筆都是 HTTP 429，模型的 06:32 那段消失，06:33–07:41 那段在後端從 06:37 開始、到 09:57（模型只到 07:41 是因為截斷），148 筆；18:53–22:40 363 → 119 筆，18 時有 357 筆失敗）；suse-observability-agent 3 → 7（01:00–06:36 的寫入全部失敗，其餘時段模型只有最近 200 筆；後端全量為 00:00–00:34、00:34–01:03、06:36–10:06、10:06、18:52–22:42、22:42–22:56、22:56–23:59，中間被短中斷切開） |

總數對帳（可以對不上的檢查）：
- `system=1`：段落 10,361 筆＋`failed` 1,815＋`failedSystem` 2,676＋`uiMaintenance` 24 = 14,876 = changemap `system=1` 的 `counts.ops` 14,876。**相同**。
- 預設畫面：段落 5,693＋`failed` 1,815 = 7,508；changemap 7,500。**不同 8 筆**：都是 `system:serviceaccount:cattle-system:cattle` 建立 Namespace `local` 回 409。changemap 依紀錄自己的 namespace（Namespace 物件是空字串）判定為系統元件；行為依目標 namespace（`local`，名稱不像 Rancher 自己的 namespace）判定為例行，所以這 8 筆計入 `failed` 而不是 `failedSystem`。逐物件比對：沒有任何物件的行為筆數多於 changemap（197 個物件 vs 207；少的 10 個物件只有失敗請求）。

### 7.2 反向檢查（對 10/30 的回應；每條都先把資料故意改壞一次，確認會報錯）

| 檢查 | 檢查了 | 違反 | 故意改壞後 |
|---|---|---|---|
| 只有 tagOnly（或低風險）警示的段落，不是要看／值得知道 | 6 段 | 0 | 把一段一般操作改成值得知道 → 報 1 |
| 例行、未記錄執行者、Rancher 未驗證請求不是要看／值得知道（警示色） | 60 段 | 0 | 把一段例行改成要看 → 報 1 |
| 沒有段落跨過當天任何一個中斷開始（含 10:06–18:53） | 70 段 × 當天全部中斷 | 0 | 把一段的結束改到中斷開始之後 → 報 2 |
| 沒有段落橫跨長中斷（≥ 60 分鐘，rke2 10:06–18:53、local 10:07–18:53） | 2 段長中斷 | 0 | （同上一條的資料） |
| 相鄰小段相隔不超過 60 分鐘 | 28 個小段間隔 | 0 | 把一個間隔改成 60 分鐘又 1 毫秒 → 報 1 |
| 回應沒有「找不到執行者」「待確認」字樣 | 整份回應 | 0 | 插入「找不到執行者」→ 報 1 |

單元測試另外蓋住任務書指定的規則，每條都把程式改壞一次確認測試變紅（7 次改壞，6 次變紅；第 7 次見第 5 節）：

| 測試 | 改壞的方式 | 結果 |
|---|---|---|
| 中斷開始的規則（中斷先寫、後寫兩種順序） | 中斷開始那一刻的操作改算之後 | 變紅 |
| 剛好 60 分鐘不斷、60 分鐘又 1 毫秒斷；晚到的紀錄把兩段接起來 | 60 分鐘改成嚴格小於 | 變紅 |
| 跨午夜同一個 id、兩天各算自己的筆數 | — | 通過 |
| Namespace 物件算自己的名稱、叢集範圍是空字串 | 拿掉這條規則 | 變紅 |
| 只有 bypassed_rancher／off_hours 不會升級；標題用 portforward | 拿掉 tagOnly 判斷 | 變紅 |
| 失敗請求依設定過濾 | 拿掉過濾 | 變紅 |
| 晚到的中斷切開既有段落 | 拿掉 `sync_gaps` 的切開 | 3 個測試變紅 |
| 重建 = 寫入時的結果 = 一次套用規則（400 筆亂序、6 段中斷穿插、Rancher 配對） | 換成 30 分鐘門檻 | 不相等（測試裡的反向斷言） |

真實資料：在 lab 資料庫副本上，把全部紀錄依到達順序（id）一筆一筆切，與依時間順序的背景建表**完全相同**（1,169 段、55,385 筆操作；測試 `real_database_arrival_order_gives_the_build`，需 `AL_TEST_DB`）。

### 7.3 回歸

| 項目 | 結果 |
|---|---|
| `/api/changemap` 新舊版逐欄位（舊版＝`fix/ui-usability` 061b85b 的 release 執行檔，同一份資料庫副本） | 10/30 預設、`system=1`、`tab=relations`、`tab=events`、`state=look`、兩座叢集各自、`namespace=nv-demo`、`namespace=argocd,nv-demo`、10/29：**全部 0 差異**（最大的一份 13,583 個欄位）。10/31：4 個差異，都是還沒結束的心跳中斷的 `toMs`（＝抓取當下的「現在」，兩次抓取相差 15 秒），不是程式差異 |
| `/api/changemap/detail`（10/30 前 15 個物件） | 0 差異 |
| `/api/stories`（10/17–10/31）、`/api/hourly` | 0 差異（stories 8,422 個欄位） |
| `/api/rules` 的 pending | 0 差異 |
| `shown.rs`、`perf.rs` 既有比對測試 | 全部通過；效能閘門（71 萬筆／天）10 個 changemap 畫面「與舊做法相同」 |
| 全部測試 | 159 通過、0 失敗（新增 13 個）；整合測試 10 通過。clippy 19 個警告（基準 21，沒有增加） |
| 開頁 5 秒內 | lab 副本冷啟動到第一個 `/api/changemap` 200：舊版 3.12／2.32／2.32／2.79 秒，新版 2.39／2.48／2.81／2.87／3.21 秒。行為表在背景建，release 1.46～1.52 秒（debug 1.83 秒） |
| 新 API 速度 | lab 10/30：`/api/behaviors` 14 ms（5 次 14.0–14.7 ms，104,666 bytes 未壓縮）；`system=1` 22–24 ms（513,038 bytes）；同時 changemap 16 ms。效能閘門 71 萬筆／天：開頁 p95 47 ms（gzip 11 KB）、含系統元件 p95 86 ms、最大一段的明細 p95 22 ms，都在目標內；從頭建表 0.7 秒 |
| 寫入速度 | 效能閘門寫入 71 萬筆：舊版 60.2 秒（11,862 筆／秒），新版 60.6 秒與 61.5 秒（11,782、11,622 筆／秒），慢約 1～2% |
| 舊頁面（權限覆核、收集狀態、規則）能開、瀏覽器截圖 | 後端 API 回歸見上；頁面截圖屬軌 2 |

## 4. 做了、但任務書沒提到的修改

- `changemap.rs`：把讀缺口的程式抽成 `day_gaps`／`gaps_json` 給兩邊共用，幾個小函式改成 `pub(crate)`。行為沒變（7.3 逐欄位 0 差異）。
- 效能閘門加了三列（行為開頁、含系統元件、最大一段明細）和從頭建表的時間。
- `gaps` 表加索引 `gaps_start (cluster, from_ms)`（切段時查「中間有沒有中斷開始」）。
- 背景建表在兩段之間暫停 5 ms：第一次在 lab 副本上跑時，建表執行緒一直搶回鎖，server 等到建完才開始服務（約 1.8 秒）。也發現與 changemap 索引的背景建立同時跑時會 `database is locked` 失敗，改成 immediate transaction 並重試 5 次。
- 回答軌 2 的問題（約定 v0.1 補充）：新增 `actor.id`（未記錄執行者是 `~<cluster>`）、`summary.look[]` 加 `fromMs`／`toMs`；不支援 `from`／`to`（介面自己留重疊的段落、不重切）；`direct` 是事實、`bypassed-rancher` 是規則；`ace` 是 Authorized Cluster Endpoint；表建好後下一次開頁就有資料。
- 已知、沿用 changemap 的規則：rke2 的未記錄執行者原因是 `found-on-reconnect`，但其中有的是 watch 即時看到的 `modified`（不是重連才發現），文字「收集中斷後重新連線比對」對這幾筆不精確。changemap 也是這樣寫，這次沒改。

## 5. 沒做完、或沒涵蓋的

- **Namespace 物件的「系統元件」判定與 changemap 差 8 筆**（7.1 對帳）：行為依目標 namespace 判斷，changemap 依紀錄的 namespace。要完全一致得把 Rancher 帳號對 Namespace 物件的操作改成依 `""` 判斷；我認為依目標 namespace 才符合第 4 節「Namespace 算它自己」，所以保留，請 Sam 決定。
- **還沒結束的心跳中斷不會切段**：要等收集元件回報、缺口寫進表之後才切開；被當成換掉的節點（retired）沒有寫進缺口表之前也不切。約定裡有寫明。
- **Rancher 那一半在建表途中才被配對**的處理（建表換上前移除）沒有測試蓋到：改壞它測試仍通過（因為一般情況在建表時已經配對好，直接略過）。重新判斷時的同一段程式沿用既有的配對修正做法。
- 拿掉的紀錄（被下游紀錄取代的 Rancher 那一半）如果剛好是接起兩段的那一筆，不會再切開；下游那一筆幾秒內就到、補上同一個位置，所以實際上不影響。
- 沒有部署到 lab（照指示由對話端部署）。瀏覽器截圖、開頁只有一個請求的網路紀錄屬軌 2。

## 6. 任務書哪裡害我走冤枉路

- 第 1 節說 10/30 「`system:cattle:error` 248 筆」與「off_hours 81 次」，前者是截斷後的數字（軌 0 已更正為 272），後者只算 stories 裡的；我一度以為後端算錯，查了才知道口徑不同。建議任務書寫數字時註明來源與口徑。
- 第 7.1 節的期望值來自截斷的明細，又沒有把「失敗請求不列入」算進去（第 3 節第 2 點只想到 4xx 的 `system:cattle:error`）。實際上 10/30 有 4,491 筆失敗寫入（1,815＋2,676），是段落差異的最大來源，比截斷還多。
- 「開頁 5 秒內」沒有說清楚量的是 changemap 還是新頁面，也沒有給舊版基準；我自己建了舊版執行檔量，才知道舊版本來就要 2.3～3.1 秒。
- 第 5 節說行為「依 `add_to_story_in` 的做法」，但 stories 只收有警示的少數紀錄、不用處理缺口晚到；行為要處理「缺口在段落之後才寫入」和「晚到的紀錄接起兩段」，這兩點任務書沒提，是寫完第一版測試才想到的。
