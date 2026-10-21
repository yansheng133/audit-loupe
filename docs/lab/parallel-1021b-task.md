# 任務書：10/21 平行三軌（第二批）——原型的正確性、讀取效能、介面配合

寫作：2026-10-21。Sam 已同意。依據：10/21 列出的效能問題與正確性問題，以及 `docs/api/changemap-v0.md` 最後的「v0.1 變更」。

## 0. 照任務書執行（Sam 10/21 要求，違反就是不合格）

上一批有人做了任務書沒寫的事（例如：把本機 server 裝成登入自動啟動的服務、改寫整套 lint 設定、改寫已推上去的分支）。**這一批不接受任何偏離。**

1. **只做本任務書列出的步驟，照編號順序做。**每一步做完，在報告的「步驟對照表」填：步驟編號、做了什麼、證據（commit、測試名稱、指令輸出摘要）。
2. **任務書沒寫的動作一律不做**，包括但不限於：
   - 安裝常駐服務、排程或背景程式。
   - 改 CI、lint、建置、相依套件的設定。
   - 改本軌範圍以外的檔案或模組。
   - 改 API 約定。
   - 刪除檔案（步驟有寫的除外）。
   - force-push、合併、打 tag、改寫歷史。
   - 碰任何叢集、在系統層級安裝任何東西。
3. **照做行不通、或任務書沒寫到的情況**：
   - 停下那一步，讓程式維持可編譯、測試會過的狀態。
   - 在報告的「卡住的地方」寫清楚：卡在哪、試過什麼、建議怎麼做。
   - 跳到下一個不依賴它的步驟。
   - **不要自己換做法、不要擴大範圍。**
4. **發現任務書寫錯**：照字面做會造成損害（資料、叢集、公開 repo）就停下來；不會造成損害就照做。兩種情況都要在報告指出。
5. 硬性限制照舊：
   - 不印也不存 token、密碼、IP。
   - Mac 不用 sudo。
   - 不開 kubeconfig 與 `env.sh` 的內容。
   - 原始資料唯讀：`~/audit-loupe-lab/overnight/`、`~/audit-loupe-lab/recordings/`。
   - 本機服務只綁 127.0.0.1。
6. 時間上限 60 分鐘。到時間就收尾：commit、push、交報告。
7. **報告最上面一定是「步驟對照表」**，列出本軌全部步驟（做了、沒做、卡住都要列），接著是「卡住的地方」，再來是「任務書沒寫但我做了的事」（應該是空的；不是空的就要說明為什麼）。

## 軌 1：原型 API 的正確性（分支 `proto/changemap`，PR #13）

指令：`讀 ~/audit-loupe/docs/lab/parallel-1021b-task.md，做「軌 1」，嚴格照步驟。`

工作目錄 `~/audit-loupe-proto`。只改 `backend/src/changemap.rs` 與它的測試；步驟 1-6 另外會動 `api.rs` 的路由與 `config` 讀環境變數的地方。

- **1-1**：收文件、清掉已合併的分支（Sam 10/21 指定由軌 1 處理）。都在 `~/audit-loupe` 主工作目錄做：
  1. 先確認 PR #12 已合併（`gh pr view 12 --json state` 要是 `MERGED`）。不是的話停下來，寫進報告。
  2. 這時主工作目錄還在 `docs/decisions-1021`，有兩個未 commit 的檔：`docs/api/changemap-v0.md`（已修改）、`docs/lab/parallel-1021b-task.md`（新檔）。`git status` 另外有別的變更的話，不要動那些變更，寫進報告。
  3. `git fetch origin`，再用 `git switch -c docs/decisions-1021b origin/main` 開新分支（這兩個檔的變更會跟著帶過去）。
  4. **只 commit 這兩個檔**，push，開 PR 到 `main`，標題 `docs: API contract v0.1 and the 10/21 second-batch task book`。
  5. 刪掉已合併的舊分支：本機 `git branch -D docs/decisions-1021`、遠端 `git push origin --delete docs/decisions-1021`。**只刪這一個分支**，Sam 10/21 指定要刪，其他分支都不准刪。
- **1-2**：**程式修改已經由 Claude 在雲端做好**（Sam 不在時，10/21），放在 `~/audit-loupe-lab/patches/track1/`，說明見同一個資料夾的 `README.md`。1-2～1-8 的內容都在這個 patch 裡，**不要自己重做**。
  - 先比對 patch 的 sha256，要等於 README 寫的值。
  - 在 `~/audit-loupe-proto` 用 `git am` 套用。
  - **套用失敗**（例如分支已經不是 `50ac250`）：用 `git am --abort` 還原，停在這一步，寫進報告。**不要手動改程式去湊。**
- **1-3**：`cargo test --locked` 與 `cargo clippy --all-targets`，數字要和 README 一致（104 通過；警告主程式 17、測試 19）。不一致就停下來，寫進報告。**一致的話馬上 push（不 force）到 `proto/changemap`**，讓 PR #13 的 CI 先跑，再做後面的步驟。最後交報告前要確認 CI 結果；CI 失敗就把失敗的項目與輸出寫進報告，不要自己改程式。
- **1-4**：用 1-9 更新後的本機 server，以真實資料驗證，結果放報告：
  - 對 10/17～10/21 每一天：
    - 翻完所有頁，總列數要等於 `counts.objects`，而且沒有重複的 key。
    - `hidden.requestOnly` 的數字要和直接用 sqlite 唯讀查 `al-copy.db` 的結果一致。
  - 有 admin 建立 `users`／`globalrolebindings`／`tokens` 的那幾天：這幾類的整批列要出現。
  - 挑一個有 exec 的 Pod，比對 `/api/changemap/detail` 與整頁回應附的 `detail`，兩者要相同；`dependsOn` 要有 SUSE Security 那一項。
  - 這一步要在 1-9 之後做；**順序改成 1-2 → 1-3（含 push）→ 1-9 → 1-10 → 1-4 → 1-11**。
- （原本的 1-5～1-8 已經包含在 patch 裡，不用做。）
- **1-9**：先在 `~/audit-loupe-proto/backend` 執行 `cargo build --release --locked`，再用 `~/audit-loupe-lab/proto/deploy-bin.sh` 更新本機 server 的執行檔（這是既有的腳本，照它原本的用法，不要改它）。確認 `127.0.0.1:18090` 的新版本回應正確：整批列出現、cursor 是不透明字串、detail API 可用。
- **1-10**：把本機 server 啟動時的 `AL_DATA_NOTE` 設成「原型：資料是 10/17–10/21 的 lab 資料庫副本，不是即時資料」。只能改 `~/audit-loupe-lab/proto/` 裡啟動 server 的設定。如果那個設定在 launchd 的 plist 裡，就改 `~/audit-loupe-lab/proto/com.sam.audit-loupe-proto.plist` 這一份，再照 STATUS.md 寫的方式重新載入。**不要新增其他服務。**
- **1-11**：push（不 force），更新 PR #13 描述，報告 `docs/lab/report-proto-changemap-v01.md`。

## 軌 2：讀取效能第一步（新分支 `perf/read-path`，從 `proto/changemap` 開，新 PR，描述第一行寫「依賴 #13」）

指令：`讀 ~/audit-loupe/docs/lab/parallel-1021b-task.md，做「軌 2」，嚴格照步驟。`

工作目錄：`git worktree add ../audit-loupe-perf -b perf/read-path origin/proto/changemap`。建好之後，先在這個 worktree 用 `git am` 套用 `~/audit-loupe-lab/patches/track1/` 的 patch（sha256 要等於 README 的值），當成第一個 commit，讓兩軌改的是同一份 `changemap.rs`。軌 1 已經把同一個 patch push 到 `proto/changemap` 的話，就直接從新的 `origin/proto/changemap` 開，不用再套。對應 `docs/layer1-plan.md` 第 8 節的 PR-1，但**只做下面列的**。

- **程式修改已經由 Claude 在雲端做好**（Sam 不在時，10/21），放在 `~/audit-loupe-lab/patches/track2/`，說明見 `README.md`。2-2～2-5 的內容都在 patch 裡，**不要自己重做**。這一軌要做的是：量改之前、套 patch、量改之後。
- **2-1：先量基準（改之前）。**
  - 在 worktree 裡只套軌 1 的 patch（上面說的），還**不要**套軌 2 的。
  - `cargo build --release --locked`。
  - 用 `audit-loupe replay` 灌一個 10 倍的資料庫：同一批 10/20 的原始 log 重播 10 次，`--shift` 分別用 `1s` 到 `10s`（auditID 會自動加後綴，不會重複）。
  - server 用你自己的埠：`127.0.0.1:18096`（ingest `18097`），資料庫放在 worktree 外的 `~/audit-loupe-lab/perf/`。**不要碰 18090、18092 與它們的資料庫。**
  - 1 倍用 `~/audit-loupe-lab/proto/db/al-copy.db` 的**副本**：18090 正開著它，所以用 `sqlite3 ~/audit-loupe-lab/proto/db/al-copy.db ".backup <目的檔>"` 複製到 `~/audit-loupe-lab/perf/`，不要直接 `cp`。
  - 1 倍、10 倍各量 `/api/changemap?day=2026-10-20` 冷、熱各 5 次：毫秒數、回應大小。原始數字放報告。
  - 另外在 server 開著的時候，同時打 5 個 `/api/changemap` 與持續重播寫入，記錄回應時間（看排隊）。
- **2-2**：比對 `~/audit-loupe-lab/patches/track2/` 的 patch sha256（要等於 README 的值），用 `git am` 套用。套用失敗：`git am --abort`，停在這一步，寫進報告，**不要手動改程式去湊**。
- **2-3**：`cargo test --locked`、`cargo clippy --all-targets`，數字要和 README 一致（107 通過；警告主程式 17、測試 19）。不一致就停下來，寫進報告。**一致的話馬上 push（不 force）`perf/read-path`，開 draft PR，base 設成 `main`**（CI 只對開向 main 的 PR 執行），描述第一行寫「依賴 #13，請先合併 #13」。之後才做 2-6 的量測。最後交報告前要確認 CI 結果；失敗就寫進報告，不要自己改程式。
- **2-4**：逐項對照 README 的「做了什麼」表，確認和這份任務書原本列的讀寫分開、gzip、Server-Timing、小表一致。對不上的寫進報告，不要補改。
- **2-5**：（併入 2-4，不用另外做。）
- **2-6**：重新 `cargo build --release --locked`，用 2-1 的同一批資料庫（**用新的副本**，讓舊資料庫的回填也被量到），照 2-1 的方法再量一次：
  - 列出改之前、改之後的對照表，含 `Server-Timing` 的 db 與 total。
  - 第一次開啟舊資料庫時的回填花了多久，也要記。
  - 用 `EXPLAIN QUERY PLAN` 證明開頭不再掃全部歷史。
  - 確認帶 `Accept-Encoding: gzip` 時有壓縮，記下壓縮前後大小。
- **2-7**：（併入 2-3，不用另外做。）
- **2-8**：push 報告（不 force），更新 PR 描述，報告 `docs/lab/report-perf-read-path.md`。

## 軌 3：介面配合 v0.1（UI repo，分支 `feat/v5`，PR #4）

指令：`讀 ~/audit-loupe/docs/lab/parallel-1021b-task.md，做「軌 3」，嚴格照步驟。`

工作目錄 `~/audit-loupe-ui-v5`。只改 v5 的檔案：`pages/ChangeMapV5.vue`、`components/v5/`、`lib/v5.js`、`styles/change-map-v5.scss`。**舊頁、lint 設定、`vue.config.js`、`scripts/dev.sh` 都不准動。**

- **3-1**：**介面修改已經由 Claude 在雲端做好**（Sam 不在時，10/21），放在 `~/audit-loupe-lab/patches/track3/`，說明見同一個資料夾的 `README.md`。原本 3-1～3-6 的內容都在這個 patch 裡，**不要自己重做**。
  - 先比對 patch 的 sha256，要等於 README 寫的值。
  - 在 `~/audit-loupe-ui-v5` 用 `git am` 套用。
  - **套用失敗**（例如分支已經不是 `bbc478c`）：`git am --abort`，停在這一步，寫進報告。**不要手動改程式去湊。**
- **3-2**：逐項對照 README 的「做了什麼」表，確認 patch 的內容和這份任務書原本的 3-1～3-6 一致（整批列、cursor 原樣帶回、設定與相關物件分開、點一列只打 detail、`meta` 提示、相容舊回應）。有對不上的，寫進報告，**不要自己補改**。
- （原本的 3-3～3-6 已經包含在 patch 裡，不用做。）
- **3-7**：`yarn lint` 與 `yarn build-pkg audit-loupe` 都要過（用既有的指令與容器做法，不改設定）。**都過了就馬上 push（不 force）`feat/v5`**，讓 PR #4 的 CI 先跑。然後確認 dev server 能啟動、編譯沒有錯誤，再停掉。不要登入 Rancher。最後交報告前要確認 CI 結果；失敗就寫進報告，不要自己改程式。
- **3-8**：軌 1 完成 1-9 之後（`curl -s -o /dev/null -w '%{http_code}' '127.0.0.1:18090/api/changemap/detail?key=x'` 回 400 代表新版已上線；回 404 代表還是舊版），用 dev server 的代理確認新欄位真的拿得到。軌 1 在你 60 分鐘內沒完成，就略過這一步並寫進報告。
- **3-9**：push（不 force），更新 PR #4 描述，報告 `docs/design/v5/report-proto-v01.md`。

## 三軌之間

- 只靠 API 約定與各自的 PR 溝通，不改別人的工作目錄。
- 本機 server（18090）只有軌 1 可以更新，而且只能在 1-9、1-10 這兩步。
