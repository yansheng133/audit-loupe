# 報告：軌 2——讀取效能第一步（`perf/read-path`）

日期：2026-10-21，14:40–15:00。任務書：`docs/lab/parallel-1021b-task.md` 的「軌 2」。

**一句話結論：兩個 patch 都套用成功，107 個測試通過；但 clippy 警告數和 README 對不上（主程式 19、測試 21，README 寫 17、19），照規定停在 2-3，沒有 push、沒有開 PR。量測顯示單一請求變快、開頭不再掃全部歷史、gzip 有效；但「同時 5 個請求＋持續寫入」在改之後明顯變慢（10 倍資料時 4–18 秒，改之前 0.4–2 秒），兩輪都重現。**

## 步驟對照表

| 步驟 | 狀態 | 做了什麼 | 證據 |
|---|---|---|---|
| 建 worktree | 完成 | `git worktree add ../audit-loupe-perf -b perf/read-path origin/proto/changemap`。`origin/proto/changemap` 還是 `50ac250`（軌 1 尚未 push），所以照任務書自己套軌 1 的 patch | `git log`：`50ac250` |
| 套軌 1 patch | 完成 | sha256 `0c79fb3a…aa02`＝README；`git am` 成功 | commit `076f818` |
| 2-1 量基準 | 完成 | `cargo build --release --locked`；1 倍用 `.backup` 複製 al-copy.db（見「任務書寫錯」第 1 點）；10 倍用 `audit-loupe replay` 重播 10/20 的兩個原始檔 10 次（`--shift 1s`～`10s`）；server 在 127.0.0.1:18096／18097；冷、熱各 5 次＋並行量測 | 第 4 節；`~/audit-loupe-lab/perf/results-before-*.tsv` |
| 2-2 套軌 2 patch | 完成 | sha256 `56a6b2cb…d06a`＝README；`git am` 成功 | commit `eb32aa6` |
| 2-3 測試、clippy、push、PR | **卡住** | `cargo test --locked`：107 通過、0 失敗、1 略過（＝README）。`cargo clippy --all-targets`：主程式 19、測試 21（README：17、19）→ 照規定停下，**沒有 push、沒有開 PR**，CI 沒有跑 | 見「卡住的地方」 |
| 2-4 對照 README | 完成 | 讀寫分開、gzip、Server-Timing、小表四項都和 README 一致；另有 3 點觀察 | 第 3 節 |
| 2-5 | 不用做 | 併入 2-4 | — |
| 2-6 量改之後 | 完成 | 重新 `cargo build --release --locked`；從 pristine 檔用 `.backup` 做新副本再量；對照表、Server-Timing、回填時間、`EXPLAIN QUERY PLAN`、gzip 前後大小 | 第 4、5 節；`results-after-*.tsv` |
| 2-7 | 不用做 | 併入 2-3 | — |
| 2-8 push、PR 描述、報告 | **部分** | 報告寫好並 commit 在本機 `perf/read-path`。**沒有 push、沒有 PR**：PR 是 2-3 成功才開的，2-3 卡住 | 本檔 |

## 卡住的地方

### 2-3：clippy 警告數和 README 不一致

- 卡在哪：README 寫「主程式 17、測試 19」，Mac 上是主程式 19、測試 21（未重複的位置共 21 個）。
- 試過什麼：在同一個 worktree 暫時切到套軌 2 之前的 `076f818` 再數一次，**也是 19、21**，數完切回 `eb32aa6`。所以軌 2 的 patch 沒有新增警告；差 2 個應該是環境不同（Mac 的 clippy 是 `0.1.99 (2026-09-28)`，雲端版本不明）。21 個警告全在既有程式（`store.rs` 的 complex type 等），`changemap.rs` 沒有。清單：`~/audit-loupe-lab/perf/clippy-after.txt`。
- 建議：若 Sam 接受「差異來自環境、patch 沒有新增警告」，執行下面兩行就完成 2-3／2-8 的 push 與 PR（**要明確寫 `origin perf/read-path`**，見「任務書寫錯」第 2 點）：
  ```sh
  git -C ~/audit-loupe-perf push -u origin perf/read-path
  gh pr create -R yansheng133/audit-loupe --draft --base main --head perf/read-path --title "perf: read path (pool, gzip, Server-Timing, small tables)" --body "依賴 #13，請先合併 #13 ..."
  ```

### 量測結果：並行＋寫入時變慢（不是步驟卡住，是要 Sam 看的結果）

- 改之後，同時 5 個請求時不再排隊（5 個幾乎同時完成），但每個都慢很多：1 倍 0.56–1.4 秒（改之前 0.05–0.29 秒），10 倍 4.2–17.8 秒（改之前 0.4–2.0 秒）。改之前、改之後交替量的第二輪也一樣。
- 單一請求、沒有並行時，改之後 10 倍的熱開頁是 0.7–1.25 秒；改之前在背景雜湊鏈檢查結束後最快 0.36 秒（兩輪的熱第 5 次）。
- 量的時候機器負載約 9–11（另外兩軌同時在編譯）。
- 原因沒有查（任務書不准改程式），推測：(1) 5 個查詢真的同時跑，在忙碌的機器上互搶 CPU；(2) 讀取連線是新開的，各自的 SQLite 快取是冷的，輪流使用讓暖身分散到 4 條；(3) 寫入時 WAL 變大，讀取要多看 WAL。建議量測時同時記錄 WAL 大小與 CPU，再決定要不要調整連線池大小或 `cache_size`／`mmap_size`。

## 任務書沒寫但我做了的事

1. 複製 1 倍資料庫時在 `sqlite3` 加了 `-readonly`：照任務書原樣的指令失敗（`unable to open database file (14)`），加上後一樣是 `.backup`，來源只讀不寫。
2. 為了判斷 clippy 差異，在 worktree 暫時 `checkout --detach 076f818`，數完切回 `perf/read-path`。工作目錄全程乾淨，沒有改任何檔案。
3. 照 2-1 的方法多量了第二輪（改之前、改之後交替），因為第一輪改之後的並行數字異常，要排除是負載造成的。
4. 為了量「回填花多久」，另外用 `.backup` 做兩份暫時副本（`bf-1x.db`、`bf-10x.db`），在上面直接執行 patch 的回填 SQL 並計時，量完刪掉。量測腳本每次也會先刪掉自己上一次做的工作副本（`work-*.db`）。刪的都是我在 `~/audit-loupe-lab/perf/` 自己建立的檔。
5. 在 `~/audit-loupe-lab/perf/` 建了量測用的檔：`serve.sh`、`measure.sh`、`concurrent.sh`、`tokens`（本機隨機 token，權限 600，沒有印出）、結果 TSV。
6. **違反硬性限制一次**：用 `lsof` 確認 18096 時少了 `-a`，輸出把其他行程的監聽位址也列出來，其中有一個內網 IP 印在對話紀錄裡（沒有存進任何檔案）。之後的指令都加了 `-a`。
7. 補充：上一批「把本機 server 裝成登入自動啟動的服務」是我（上一批的軌 B）做的，這一批沒有動 18090、18092 與它們的資料庫。

## 任務書寫錯或沒寫到的地方

1. **`sqlite3 <db> ".backup <目的檔>"` 在這台 Mac 會失敗**（error 14，來源要以讀寫模式開）。不會造成損害，加 `-readonly` 即可。
2. **`git worktree add ... origin/proto/changemap` 會把 `perf/read-path` 的上游設成 `origin/proto/changemap`**。之後只打 `git push` 會推到軌 1 的分支，可能造成損害。push 時一定要寫 `git push -u origin perf/read-path`。
3. **10 倍資料庫要以什麼為底沒寫**。我用空的資料庫，所以 10 倍只有下游 apiserver 紀錄（10/20 共 649,130 筆＝64,913×10），沒有 Rancher 紀錄、local 叢集、物件版本。1 倍是完整的 al.db 副本，兩者內容不完全同類。
4. clippy 數字見「卡住的地方」。

## 1. 套用的 commit

| commit | 內容 |
|---|---|
| `076f818` | 軌 1 patch（changemap v0.1） |
| `eb32aa6` | 軌 2 patch（讀取連線池、gzip、Server-Timing、小表） |
| （本報告） | `docs/lab/report-perf-read-path.md` |

## 2. 資料庫

| | 檔案 | 大小 | events |
|---|---|---|---|
| 1 倍 | `~/audit-loupe-lab/perf/pristine-1x.db`（al-copy.db 的 `.backup`） | 159,920,128 B | 217,127（10/20：66,541） |
| 10 倍 | `~/audit-loupe-lab/perf/pristine-10x.db`（空資料庫＋重播 10 次，65 秒） | 612,929,536 B | 830,460（10/20：649,130；auditID 全不重複；integrity_check ok） |

每次量測都從 pristine 檔用 `.backup` 做新的工作副本，pristine 檔本身沒有給 server 開過（10 倍的 pristine 是灌資料的 server 寫出來的）。

## 3. 2-4：README「做了什麼」對照

| 項目 | README | 實際 patch | 一致 |
|---|---|---|---|
| 讀寫分開 | 讀取 API 從連線池（`AL_READ_CONNS`，預設 4）；收資料、心跳、`/api/rules`、重新計算用寫入那條 | `AppState::read()` 輪流取池裡的連線；`api.rs` 仍用 `s.store` 的只有收資料（含 security、heartbeat）、`rule_status`、`start_recompute` | 是 |
| gzip | 只加 tower-http 的 `compression-gzip`，`Cargo.lock` 不變 | `Cargo.toml` 一行（加 feature）；`CompressionLayer` 加在讀取 router；`--locked` 編譯通過 | 是 |
| Server-Timing | changemap 與 detail 有 `db` 與 `total`，其他讀取 API 有 `total` | `with_db_timing`＋`server_timing` middleware | 是 |
| 小表 | `cluster_stats`、`resource_kinds`，trigger 維護，舊資料庫回填一次 | `prepare_stats`；`changemap.rs` 的三個全歷史查詢換成讀這兩張表；一致性測試 `stats_tables_match_the_history` | 是 |

觀察（不補改）：

- `Cargo.toml` 有改（加一個 feature）。README 有說明，屬於任務書要求的 gzip，但碰到「相依套件的設定」，提出來給 Sam 知道。
- `AL_READ_CONNS` 沒有寫進 `docs/` 或 Helm chart。
- 小表只靠 INSERT trigger；保留期刪除舊資料時不會更新。因為存的是「最新」時間，平常不影響；只有某座叢集的資料全部被刪光時，`asOf` 會留著舊值。

## 4. 量測（2-1、2-6）

條件：Mac（Apple Silicon）、release build、`/api/changemap?day=2026-10-20`（overview、全部叢集、不含系統元件）。冷＝重啟 server 後第一個請求。量的時候機器負載約 9–11。

- 10 倍熱開頁「改之前」第 5 次突然降到約 360 毫秒、回應少 5 位元組：背景的雜湊鏈檢查剛做完（`hashChain` 從 `pending` 變 `ok`）。前 4 次在和檢查搶 CPU。
- 並行測試：每輪同時送 5 個請求，同時用 `replay --shift 11s` 持續寫入。改之後的 10 倍測試總共約 35 秒，重播在第 1 輪就寫完；改之前測試約 6 秒，重播一直在寫。**測試長度不同，這不能拿來說寫入變快。**
- 回應大小沒有壓縮（未帶 `Accept-Encoding`）。

#### 第一輪：單一請求

| 版本 | 資料量 | 冷 1–5（毫秒） | 熱 1–5（毫秒） | 回應大小（位元組） | Server-Timing（熱 5 次，db／total 毫秒） |
|---|---|---|---|---|---|
| 改之前 | 1 倍 | 122、167、148、140、175 | 166、136、141、106、106 | 72722 | — |
| 改之後 | 1 倍 | 67、76、71、63、66 | 78、81、84、95、78 | 72722 | 77.2／77.4；80.4／80.6；82.8／82.9；94.3／94.5；77.2／77.4 |
| 改之前 | 10 倍 | 2157、2178、1463、1535、1725 | 1560、1253、1175、1125、363 | 107980、107985 | — |
| 改之後 | 10 倍 | 887、1058、1005、986、935 | 913、923、839、1143、1156 | 107985 | 911.6／911.8；922.1／922.4；837.7／837.9；1141.7／1142.0；1155.2／1155.5 |

#### 第一輪：同時 5 個請求＋持續重播寫入（3 輪，每輪 5 個，毫秒）

| 版本 | 資料量 | 第 1 輪 | 第 2 輪 | 第 3 輪 | 重播還在寫的輪次 |
|---|---|---|---|---|---|
| 改之前 | 1 倍 | 67、115、160、250、206 | 49、95、194、287、241 | 61、157、111、254、207 | 1、2、3 |
| 改之後 | 1 倍 | 766、811、808、797、849 | 613、561、623、621、679 | 1304、1307、1312、1306、1405 | 1、2、3 |
| 改之前 | 10 倍 | 445、1203、817、1586、2009 | 427、813、1595、1208、1982 | 438、1210、819、1591、2000 | 1、2、3 |
| 改之後 | 10 倍 | 12710、12799、12802、12816、13371 | 17433、17472、17476、17837、17478 | 4362、4361、4371、4744、4370 | 1 |

#### 第二輪（改之前、改之後交替量）：單一請求

| 版本 | 資料量 | 冷 1–5（毫秒） | 熱 1–5（毫秒） | 回應大小（位元組） | Server-Timing（熱 5 次，db／total 毫秒） |
|---|---|---|---|---|---|
| 改之前 | 1 倍 | 142、126、174、162、129 | 139、158、142、104、99 | 72722 | — |
| 改之後 | 1 倍 | 68、85、90、72、72 | 75、80、71、67、69 | 72722 | 73.8／74.0；78.6／78.8；68.2／68.3；66.2／66.3；68.6／68.8 |
| 改之前 | 10 倍 | 1505、1492、1433、1385、1387 | 1409、1450、1200、1141、361 | 107980、107985 | — |
| 改之後 | 10 倍 | 993、1046、960、883、918 | 892、1059、730、1226、1253 | 107985 | 891.2／891.5；1057.7／1057.9；728.6／728.9；1225.3／1225.6；1251.7／1251.9 |

#### 第二輪（改之前、改之後交替量）：同時 5 個請求＋持續重播寫入（3 輪，每輪 5 個，毫秒）

| 版本 | 資料量 | 第 1 輪 | 第 2 輪 | 第 3 輪 | 重播還在寫的輪次 |
|---|---|---|---|---|---|
| 改之前 | 1 倍 | 60、105、149、195、242 | 73、120、257、211、165 | 53、101、149、241、195 | 1、2、3 |
| 改之後 | 1 倍 | 687、637、690、689、739 | 1247、1246、1248、1307、1247 | 901、910、910、871、971 | 1、2、3 |
| 改之前 | 10 倍 | 372、739、1881、1152、1516 | 422、794、1552、1173、1929 | 421、1971、1589、805、1190 | 1、2、3 |
| 改之後 | 10 倍 | 12858、13458、12899、12887、12902 | 17169、17171、17160、17157、17555 | 4199、4173、4199、4579、4170 | 1 |


## 5. 2-6 其他項目

### 回填花多久

| 量法 | 1 倍 | 10 倍 |
|---|---|---|
| 在新副本上直接執行 patch 的回填 SQL（建表＋三個 INSERT） | 約 15 毫秒 | 約 41 毫秒 |
| server 從啟動到 `/healthz` 回應（新副本、第一次開，含回填） | 改之後 496 毫秒（第一輪）、126 毫秒（第二輪）；改之前 132、128 毫秒 | 改之後 130、128 毫秒；改之前 133 毫秒 |

回填本身在 50 毫秒內，啟動時間的差異落在雜訊範圍。

### `EXPLAIN QUERY PLAN`（10 倍工作副本，唯讀）

| 查詢 | 計畫 | 時間（10 倍／1 倍） |
|---|---|---|
| 舊：`SELECT cluster, max(ts_ms) FROM events GROUP BY cluster` | `SCAN events USING COVERING INDEX events_pair`（整個索引） | 240／192 毫秒 |
| 舊：`SELECT cluster, max(observed_ms) FROM versions GROUP BY cluster` | `SCAN versions USING INDEX versions_kind` | 0.2／26 毫秒 |
| 舊：`SELECT DISTINCT cluster, api_group, resource, kind FROM versions` | `SCAN versions USING INDEX versions_obj`＋`TEMP B-TREE FOR DISTINCT` | 0.1／1.2 毫秒 |
| 新：`cluster_stats` | `SCAN cluster_stats`（每座叢集一列） | 0.02／0.1 毫秒 |
| 新：`resource_kinds` | `SCAN resource_kinds`（1 倍 34 列） | 0.1／0.1 毫秒 |

開頭不再掃 events 的全部歷史。10 倍沒有物件版本，所以 versions 的兩個舊查詢在 10 倍很快。

### gzip

| | 未壓縮 | 帶 `Accept-Encoding: gzip` | `Content-Encoding` |
|---|---|---|---|
| 1 倍 | 72,767 B | 7,252 B | gzip（未帶時沒有這個標頭） |
| 10 倍 | 112,023 B | 10,131 B | gzip |

1 倍解壓後與未壓縮的內容相同（`counts` 與列數一致）。

## 6. 清理狀態

- 18096／18097 的 server 已停，沒有在監聽。
- 18090、18092 與它們的資料庫沒有碰。
- `~/audit-loupe-lab/perf/` 留著 pristine 資料庫、工作副本、腳本、TSV，供 Sam 重量。要刪由 Sam 決定。
