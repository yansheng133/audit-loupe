# 任務書：10/21 平行三軌——一小時內在 Mac 上看到 v5（用真實資料）

寫作：2026-10-21。Sam 已同意。取代 `batch-1021-task.md`（那份不執行，只當之後完整版的參考）。
目標：**一小時內**，Sam 在 dev server 登入 lab 的 Rancher，就能用 10/18–10/20 的真實資料點 v5。**不碰 lab 叢集**，不升級、不改設定。

三軌由三個 CLI 同時做，各在自己的工作目錄（git worktree），彼此不等。

## 0. 共同規則

- **不碰任何叢集**；不合併 PR；不打 tag；不推映像檔；不 force-push。
- 硬性限制照舊：不印也不存 token／密碼／IP、Mac 不用 sudo、不開 kubeconfig 與 `env.sh` 的內容、不讀任何憑證、本機服務只綁 127.0.0.1。
- 原始資料一律唯讀：`~/audit-loupe-lab/overnight/`、`~/audit-loupe-lab/recordings/`。要寫就寫副本。
- **三軌之間只靠下面這些檔案溝通，不互相改對方的工作目錄**：
  - API 約定：`~/audit-loupe/docs/api/changemap-v0.md`（三軌都以它為準，要改就在報告裡提，不要自己改）。
  - 軌 B 的產出：`~/audit-loupe-lab/proto/`（`fixture-*.json`、`STATUS.md`）。
- **時間到 60 分鐘就收尾**：可以編譯、測試會過的狀態，commit、push，寫報告。沒做完的寫清楚。

## 軌 A：PR #11 補強

- 指令：`讀 ~/audit-loupe/docs/lab/parallel-1021-task.md，做「軌 A」。`
- **先做（在 `~/audit-loupe` 主工作目錄）**：
  - `pr11-followup-task.md` 第 0 節的事：把未 commit 的文件收進 PR #10 或新分支。
  - 這次新增的文件也一起收：`docs/api/changemap-v0.md`、本任務書、`batch-1021-task.md`。
- **然後**：`git worktree add ../audit-loupe-pr11 fix/collector-rotation`，在那裡照 `docs/lab/pr11-followup-task.md` 做。
- 60 分鐘內做不完：先做第 1 節（Sam 的三個決定）與第 2 節第 1、2、3 項，其他寫進報告。
- 報告：`docs/lab/report-pr11-followup.md`（放在 PR #11）。

## 軌 B：重播工具＋`/api/changemap` v0＋本機 server

- 指令：`讀 ~/audit-loupe/docs/lab/parallel-1021-task.md，做「軌 B」。`
- **工作目錄**：`git worktree add ../audit-loupe-proto -b proto/changemap origin/main`，只在這裡改。
- **依序做**：
  1. **重播工具**（15 分鐘內先有能用的版本）：讀原始 audit log，經正式的 ingest 路徑送進本機 server。
     - 選項：叢集名稱、不等待、時間平移（可先不做）。
     - 原檔唯讀。
     - 用它把 `~/audit-loupe-lab/overnight/raw/` 與 `recordings/` 灌進一個**本機 server**（新的資料庫，放在 `~/audit-loupe-lab/proto/db/`）。
  2. **另一份資料來源**：本機 server 也可以直接開 `~/audit-loupe-lab/overnight/db/al.db` 的**副本**（含 local 叢集、規則警示、物件版本）。兩種你選一種當 Sam 看的資料，選資料比較完整的那種，寫進 `STATUS.md`。
  3. **實作 `/api/changemap` v0**：照 `docs/api/changemap-v0.md`。v0 直接查現有資料表，不做彙總表；寫幾個基本測試。
  4. **本機 server 常駐**：跑在 `127.0.0.1:18090`。把啟動指令、資料來源、哪幾天有資料寫進 `~/audit-loupe-lab/proto/STATUS.md`。
  5. **產生範例檔**：用 `/api/changemap` 對有資料的那幾天各存一份 `fixture-<day>-<tab>.json` 到 `~/audit-loupe-lab/proto/`，讓軌 C 不必等 server。**第一份範例檔盡早產出**（目標 25 分鐘內），之後再更新。
- PR 開在 main repo，標題前加 `[原型]`，不合併。
- 報告：`docs/lab/report-proto-changemap.md`（放在這個 PR）。內容含 v0 開頁的毫秒數（冷、熱各 5 次），只當參考。

## 軌 C：v5 介面

- 指令：`讀 ~/audit-loupe/docs/lab/parallel-1021-task.md，做「軌 C」。`
- **工作目錄**：在 `~/audit-loupe-ui` 執行 `git worktree add ../audit-loupe-ui-v5 -b feat/v5 origin/main`，只在這裡改。
- **規格**：`~/audit-loupe-ui/docs/design/v5/`（`spec.md`、三張截圖、設計稿原始碼）。這個資料夾還沒 commit，先複製到 worktree 再 commit。
- **資料**：照 `docs/api/changemap-v0.md` 讀。
  - 軌 B 的範例檔出來之前，先做版面與元件，不要自己編資料塞進畫面。
  - 範例檔出來後，用範例檔接上。
  - 軌 B 的本機 server 起來後，接本機 server。
- **讓 dev server 接本機 server**：
  - 只改開發設定，extension 的程式不改。
  - 例如在 dev server 的代理設定加一條規則，把 Audit Loupe service proxy 的路徑 `/k8s/clusters/local/api/v1/namespaces/audit-loupe-system/services/http:audit-loupe:8080/proxy/` 轉到本機 server；其他路徑照舊轉給 lab 的 Rancher。
  - dev server 跑在容器裡：怎麼連到 Mac 上 `127.0.0.1:18090` 由你確認（例如 Rancher Desktop 的 `host.docker.internal`）。連不到時，可以把本機 server 也跑成容器、放在同一個網路。**不要讓任何服務對外開放。**
  - 這條規則要有開關（例如環境變數 `AL_LOCAL_API`），預設關閉；正式 build 不受影響。
- **範圍**：60 分鐘內的優先順序：
  1. 總覽分頁＋右側物件細節＋底部時間列。
  2. 事件分頁。
  3. 關係分頁。
  - 做不完的放後面，寫進報告。
- **驗證**：lint 與 build 要過。dev server 能啟動、編譯沒有錯誤後停掉，不要自己登入 Rancher。
- PR 開在 UI repo，標題前加 `[原型]`，不合併。
- **報告**：`docs/design/v5/report-proto.md`（放在這個 PR），寫明 Sam 要執行的指令（含 `AL_LOCAL_API` 的設定）與要開的網址。

## 三軌都做完之後

- 軌 C 的報告最後要寫：用軌 B 的本機 server 開 v5 的完整步驟，讓 Sam 照著做就能看到。
- 有任何一軌沒做完，其他兩軌的報告照常交。
