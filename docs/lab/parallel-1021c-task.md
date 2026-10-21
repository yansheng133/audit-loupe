# 任務書：10/21 第三批——修正上一批找到的問題（軌 A）、查讀取變慢的原因（軌 B）

寫作：2026-10-21。Sam 已同意。依據：上一批三軌的報告，以及 #14、#13、#11 合併後的 main（`d20e66a`）。

## 0. 照任務書執行（Sam 10/21 要求，違反就是不合格）

1. **只做本任務書列出的步驟，照編號順序做。**每一步做完，在報告的「步驟對照表」填：步驟編號、做了什麼、證據（commit、測試名稱、指令輸出摘要）。
2. **任務書沒寫的動作一律不做**，包括但不限於：
   - 安裝常駐服務、排程或背景程式。
   - 改 CI、lint、建置、相依套件的設定。
   - 改程式、改補丁內容（這一批的程式修改都在補丁裡，已經寫好）。
   - 改 API 約定。
   - 刪除檔案或分支（步驟有寫的除外）。
   - force-push、合併、打 tag、改寫歷史。
   - 碰任何叢集、在系統層級安裝任何東西。
3. **照做行不通、或任務書沒寫到的情況**：
   - 停下那一步，讓程式維持可編譯、測試會過的狀態。
   - 在報告的「卡住的地方」寫清楚：卡在哪、試過什麼、建議怎麼做。
   - 跳到下一個不依賴它的步驟。
   - **不要自己換做法、不要擴大範圍。**
4. **發現任務書寫錯**：照字面做會造成損害（資料、叢集、公開 repo）就停下來；不會造成損害就照做。兩種情況都要在報告指出。
5. 硬性限制照舊：
   - 不印也不存 token、密碼、IP（也不要用會印出 IP 的指令，例如 `lsof -i`、`ifconfig`）。
   - Mac 不用 sudo。
   - 不開 kubeconfig、`env.sh`、`~/audit-loupe-lab/perf/tokens` 的內容。
   - 原始資料唯讀：`~/audit-loupe-lab/overnight/`、`~/audit-loupe-lab/recordings/`、`~/audit-loupe-lab/perf/pristine-*.db`。
   - 本機服務只綁 127.0.0.1。
   - **CLI 不合併 PR**，由 Sam 合併。
6. 補丁一律先比對 SHA-256（`shasum -a 256`），要等於 README 寫的值，再用 `git am` 套用。套用失敗就 `git am --abort`，停在那一步。**不要手動改程式去湊。**
7. clippy 的規則：**警告數不比基準多**就算通過（Mac 和雲端的 clippy 版本不同，絕對數字不會一樣）。基準在各軌的步驟裡先量。
8. 開新分支後第一次 push 一律用 `git push -u origin <分支名>`，不要依賴自動推斷的上游。
9. 報告最上面一定是「步驟對照表」，列出本軌全部步驟（做了、沒做、卡住都要列），接著是「卡住的地方」，再來是「任務書沒寫但我做了的事」（應該是空的；不是空的就要說明為什麼）。
10. 時間上限：軌 A 60 分鐘、軌 B 90 分鐘。到時間就收尾交報告。

## 軌 A：套用修正、push、更新本機 server、用真實資料驗證

指令：`讀 ~/audit-loupe/docs/lab/parallel-1021c-task.md，做「軌 A」，嚴格照步驟。`

補丁與說明：`~/audit-loupe-lab/patches/trackA/`（先讀 `README.md`）。

- **A-1：收任務書。**在 `~/audit-loupe` 主工作目錄：
  1. `git status`：應該只有這份任務書是新檔（untracked）。有別的變更就不要動那些變更，寫進報告。
  2. `git fetch origin`，再 `git switch -c docs/task-1021c origin/main`（新檔會跟著帶過去）。
  3. **只 commit 這份任務書**，`git push -u origin docs/task-1021c`，開 PR 到 `main`，標題 `docs: 10/21 third-batch task book`。
  4. 不刪任何分支。
- **A-2：開工作目錄、量基準。**
  1. 在 `~/audit-loupe`：`git worktree add ../audit-loupe-a -b fix/changemap-v01-followup d20e66af70e2f91d8eb1f2a28a0ccbdf1cd10322`。
  2. 在 `~/audit-loupe-a/backend`：`cargo clippy --all-targets 2>&1 | tee ~/audit-loupe-lab/patches/trackA/clippy-main.txt`，記下主程式、測試各幾個警告。
- **A-3：套後端補丁。**比對 `backend-0001-changemap-exec-over-GET.patch` 的 SHA-256，在 `~/audit-loupe-a` 用 `git am` 套用。
- **A-4：測試、push、開 PR。**
  1. 在 `~/audit-loupe-a/backend`：`cargo test --locked`，要是單元 114 通過、整合 10 通過、0 失敗。不一致就停下來，寫進報告。
  2. `cargo clippy --all-targets`：警告數不比 A-2 多。多了就停下來，寫進報告。
  3. 都通過就 `git push -u origin fix/changemap-v01-followup`，開 PR 到 `main`，標題 `changemap v0.1 follow-up: exec over GET, detail 404 says no-records`，描述貼 README 的「後端」表格。
- **A-5：套介面補丁。**在 `~/audit-loupe-ui-v5`：
  1. 確認分支是 `feat/v5`、`git rev-parse HEAD` 是 `4469c8dbff4092bd1a09c2cfcfe5c1666da87e88`、`git status` 乾淨。任一項不對就停下來，寫進報告。
  2. 比對 `ui-0001-v5-note-row-colours-no-records.patch` 的 SHA-256，用 `git am` 套用。
- **A-6：介面檢查、push。**`yarn lint` 與 `yarn build-pkg audit-loupe` 都要通過（用既有的指令與容器做法，不改設定）。都過了就 `git push origin feat/v5`（不 force），讓 PR #4 的 CI 跑。
- **A-7：更新本機 server。**
  1. 在 `~/audit-loupe-a/backend`：`cargo build --release --locked`。
  2. `~/audit-loupe-lab/proto/deploy-bin.sh ~/audit-loupe-a/backend/target/release/audit-loupe`（把執行檔路徑當參數傳進去；**不要改這個腳本**）。要看到 `both up`。
- **A-8：用真實資料驗證**（結果放報告；回應內容只摘要需要的欄位）：
  1. 沒紀錄的物件：
     `curl -s -w ' %{http_code}\n' '127.0.0.1:18090/api/changemap/detail?day=2026-10-20&key=none//pods/none/none'`
     要得到 `{"error":"no-records"} 404`。
  2. 找 10/20 有進入容器的 Pod（唯讀）：
     `sqlite3 -readonly ~/audit-loupe-lab/proto/db/al-copy.db "SELECT cluster, namespace, name, verb, subresource, count(*) FROM events WHERE resource='pods' AND subresource IN ('exec','attach','portforward') AND date(ts_ms/1000,'unixepoch','+8 hours')='2026-10-20' GROUP BY 1,2,3,4,5 ORDER BY 6 DESC LIMIT 5"`
     10/20 沒有的話，依序改查 10/18、10/19、10/17，用第一個有資料的那天，寫進報告。
  3. 挑第 2 步的第一列，key 是 `<cluster>//pods/<namespace>/<name>`，用
     `curl -s --get --data-urlencode 'day=<那天>' --data-urlencode 'key=<key>' 127.0.0.1:18090/api/changemap/detail`
     確認：`ops` 裡有 `verb` 是 `exec`（或 `attach`／`portforward`，跟資料庫一致）的操作，而且資料庫裡那幾筆原本的 verb 是 `get` 或 `create`；`dependsOn[0].setting` 是「是否啟用 SUSE Security 收集」。用 `python3 -c` 解析 JSON，只印這幾個欄位。`ops` 是空的（執行者可能被歸為系統元件而預設隱藏），就加上 `--data-urlencode 'system=1'` 再查一次，寫進報告。
  4. 同一個 key，`/api/changemap?day=<那天>&select=<key>`（一樣用 `--data-urlencode`；第 3 步有加 `system=1` 的話這裡也加）回應裡的 `detail`，`ops` 與 `dependsOn` 要和第 3 步相同。
- **A-9：介面能啟動。**用既有的 dev server 做法，確認編譯沒有錯誤，再停掉。不要登入 Rancher。停掉之後執行 `touch ~/audit-loupe-lab/perf/.trackA-done`（告訴軌 B 可以開始量測；這是本軌唯一要寫進 `perf/` 的東西）。
- **A-10：報告。**
  1. 報告寫在 `~/audit-loupe-a/docs/lab/report-trackA-1021c.md`，commit，`git push`（不 force）。
  2. 等 A-4 的 PR 與 PR #4 的 CI 跑完，結果寫進報告後再 push 一次。CI 失敗就把失敗的項目與輸出寫進報告，不要自己改程式。
  3. 告訴 Sam 怎麼在瀏覽器看介面（用既有的 `scripts/dev.sh` 做法，`AL_LOCAL_API` 指向 18090），但不要替他開。

## 軌 B：讀取變慢的原因（只量測，不改程式、不 push）

指令：`讀 ~/audit-loupe/docs/lab/parallel-1021c-task.md，做「軌 B」，嚴格照步驟。`

補丁、腳本與說明：`~/audit-loupe-lab/patches/trackB/`（先讀 `README.md`，裡面有要回答的問題和假設）。

- **B-1：放好腳本。**比對 `~/audit-loupe-lab/patches/trackB/concurrency-matrix.sh` 的 SHA-256，複製到 `~/audit-loupe-lab/perf/concurrency-matrix.sh`，`chmod +x`。**不要改腳本內容。**
- **B-2：開工作目錄、套軌 A 後端補丁、做第一個執行檔。**
  1. 在 `~/audit-loupe`：`git fetch origin`，再 `git worktree add ../audit-loupe-perf2 -b perf/read-path-v2 d20e66af70e2f91d8eb1f2a28a0ccbdf1cd10322`。
  2. 比對 `~/audit-loupe-lab/patches/trackA/backend-0001-changemap-exec-over-GET.patch` 的 SHA-256，用 `git am` 套用。
  3. 在 `backend`：`cargo clippy --all-targets`，記下警告數（B-3 的基準）。
  4. `cargo build --release --locked`，再 `rm -f ~/audit-loupe-lab/perf/bin/audit-loupe-mainA && cp target/release/audit-loupe ~/audit-loupe-lab/perf/bin/audit-loupe-mainA`。
- **B-3：套讀取路徑補丁、做第二個執行檔。**
  1. 比對 `0001-read-path-pool-gzip-server-timing-stats.patch` 的 SHA-256，用 `git am` 套用。
  2. `cargo test --locked`：單元 117 通過、整合 10 通過、0 失敗。不一致就停下來，寫進報告。
  3. `cargo clippy --all-targets`：警告數不比 B-2 多。
  4. `cargo build --release --locked`，再 `rm -f ~/audit-loupe-lab/perf/bin/audit-loupe-pool && cp target/release/audit-loupe ~/audit-loupe-lab/perf/bin/audit-loupe-pool`。
- **B-4：帶上舊報告。**舊分支 `perf/read-path` 的最後一個 commit `5a454bc` 是上一批的報告。先 `git show --stat 5a454bc`：
  - 只動 `docs/lab/report-perf-read-path.md` 的話，在 `~/audit-loupe-perf2` 執行 `git cherry-pick 5a454bc`。
  - 還動了別的檔案，就不要 cherry-pick，寫進報告。
- **B-5：等機器空下來。**量測要在其他 CLI 沒在編譯時做，數字才有意義。
  1. `df -h ~`：可用空間要至少 8 GB（12 份工作副本）。不夠就停下來，寫進報告。
  2. 每 30 秒檢查一次，兩個條件都成立才往下：`~/audit-loupe-lab/perf/.trackA-done` 存在；`pgrep -fl 'cargo|rustc|vue-cli-service|webpack'` 沒有結果。
  3. 最多等 45 分鐘。等到時間還不成立，照樣往下做，在報告寫明當時哪個條件不成立。
  4. 記下 `uptime` 與 `sysctl -n hw.ncpu`。
- **B-6：跑 12 組量測。**在 `~/audit-loupe-lab/perf/`，**照下面順序一組一組跑**（同時只跑一組）。每組的標準輸出存成 `results-matrix-<標籤>.tsv`：

  | # | 執行檔 | 原始資料庫 | 標籤 | 讀取連線數 | 寫入 |
  |---|---|---|---|---|---|
  | 1 | `bin/audit-loupe-mainA` | `pristine-1x.db` | `mainA-1x-wno` | `-` | `no` |
  | 2 | `bin/audit-loupe-pool` | `pristine-1x.db` | `pool1-1x-wno` | `1` | `no` |
  | 3 | `bin/audit-loupe-pool` | `pristine-1x.db` | `pool4-1x-wno` | `4` | `no` |
  | 4 | `bin/audit-loupe-mainA` | `pristine-1x.db` | `mainA-1x-wyes` | `-` | `yes` |
  | 5 | `bin/audit-loupe-pool` | `pristine-1x.db` | `pool1-1x-wyes` | `1` | `yes` |
  | 6 | `bin/audit-loupe-pool` | `pristine-1x.db` | `pool4-1x-wyes` | `4` | `yes` |
  | 7–12 | 同 1–6 | `pristine-10x.db` | 把 `1x` 換成 `10x` | 同 1–6 | 同 1–6 |

  例：`./concurrency-matrix.sh bin/audit-loupe-pool pristine-10x.db pool4-10x-wyes 4 yes > results-matrix-pool4-10x-wyes.tsv`

  某一組失敗（例如 server 起不來）：記下 `logs/server.log` 最後 20 行（先確認裡面沒有 token 或 IP），跳下一組。
- **B-7：整理。**只整理數字，**不改程式、不試修法**。報告要有：
  1. 一張總表，每組一列：每輪 5 個請求的最短／最長秒數、`Server-Timing` 的 db、WAL 最大與結束大小（`logs/summary-*.txt`）、有寫入時的 `checkpoint_passive` 結果、各輪期間 server CPU 的平均與最大（`logs/sample-*.tsv`）、開始與結束的 load。
  2. 10 倍、`pool4`、有寫入那組的 WAL 大小對時間（從 `logs/sample-pool4-10x-wyes.tsv` 摘每 5 秒一筆即可）。
  3. 對 README 的四個假設，各寫「數字支持／不支持／看不出來」，並指出是哪幾格數字。不要寫數字以外的推論。
  4. 腳本或量測方式本身有問題的地方（例如某個欄位抓不到），照實寫。
- **B-8：收尾。**
  1. 刪掉這一批產生的工作副本：`~/audit-loupe-lab/perf/` 裡的 `work-mainA-*`、`work-pool1-*`、`work-pool4-*`（含 `-wal`、`-shm`）。**只刪這三種前綴**，其他檔案（包括上一批的 `work-before-*`、`work-after-*`）都不要動。
  2. 報告寫在 `~/audit-loupe-perf2/docs/lab/report-perf-concurrency.md`，在 `perf/read-path-v2` commit。**這一軌不 push**：這個分支要等軌 A 的 PR 合併後才整理成 PR。

## 兩軌之間

- 軌 A、軌 B 可以同時開始。軌 B 在 B-5 等軌 A 的 A-9 做完才開始量測。
- 本機 server（18090）只有軌 A 可以更新，而且只能在 A-7。軌 B 只用 18096／18097。
- 不改別人的工作目錄。
