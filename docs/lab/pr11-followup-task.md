# 任務書：PR #11（收集元件修正）合併前的補強

寫作：2026-10-21。Sam 已同意：修完以下各項再由 Sam 合併。**不合併、不發版。**本任務書是 `parallel-1021-task.md` 的軌 A。**不碰 lab。**
依據：`docs/change-map-requirements.md` 第 3.6 節與第 4 節 10/21 追加的第 7、8 點。另有一份獨立的程式審查（下面第 2 節），審查用的是 PR 分支 commit `32df4e8`，行號以那時為準。

## 0. 先處理工作目錄

- `~/audit-loupe` 目前在 `lab/overnight-1020`，有一個未 commit 的修改：`docs/change-map-requirements.md`（10/21 的決定與效能評估）。
  - PR #10 還沒合併：commit 到 `lab/overnight-1020` 並 push（跟著 PR #10）。
  - PR #10 已合併：開分支 `docs/decisions-1021`，commit、push、開 PR。
- 本任務書、`parallel-1021-task.md`、`batch-1021-task.md`、`docs/api/changemap-v0.md` 也一起 commit 進去。
- 然後切到 `fix/collector-rotation` 做下面的事。

## 1. Sam 10/21 的決定（要做進 PR #11）

1. **首次安裝讀不讀現有的 `audit.log`：可設定，預設不讀。**
   - Helm values 加一個設定（名稱自訂，例如 `collector.file.onFirstStart: fromNow | readExisting`），預設 `fromNow`。
   - `fromNow`：從目前 `audit.log` 結尾開始，送一筆「首次啟動」的紀錄（不是缺口，是起點標記），讓畫面知道「從這一刻開始才有資料」。
   - `readExisting`：讀目前的 `audit.log`（輪替出去的舊檔不讀），這些紀錄要標成「安裝前既有」，跟即時收到的分得出來（server 存得下來、API 回得出來即可，畫面之後做）。
   - values 的註解與 `docs/deploy.md` 要寫清楚開啟的影響：安裝前的紀錄不是即時收的、舊檔不會讀所以可能不完整、首次啟動會一次讀進最多約 100 MB。
   - **狀態檔遺失不等於首次安裝**：`/var/lib/audit-loupe` 被刪或遺失，但 server 已經有這個節點的紀錄時，要當成重新啟動處理：從現在開始讀、回報缺口，原因寫「收集元件狀態遺失」。怎麼判斷由你設計，寫進 PR 描述。`deploy.md:235` 說可以刪這個目錄的地方要加上這個影響。
2. **自己的失敗也要顯示**：心跳中斷（server 判定收集元件沒有回報）和收集元件主動回報的缺口重疊時，**兩種都留、都由 `/api/coverage` 回傳**，各自有類型與原因，不互相合併或蓋掉。現有的心跳缺口判定照舊。
3. **被擱置的行要查得到**：`/api/coverage` 回傳各收集元件被擱置的行數；另加一支 API 列出被擱置的行（時間、叢集、節點、原因、檔名與位置、內容的 sha256；**不回傳行的原文**，原文可能含 request body）。畫面之後在 v5 做，這次只做 API。

## 2. 審查發現要修的（依嚴重度）

1. **輪替瞬間的認檔競爭**（`collect.rs:187-201` 的 `in_write_order`／`locate`，以及 `:399`、`:417`、`:419`）
   - 列目錄和檢查 `audit.log` 是分開兩步；中間剛好輪替時，舊 inode 找不到，被誤判成「被換掉」，跳到新檔的 0，剛輪替出去的檔沒讀完。審查時用 sleep 放大這個時間窗，8 次有 6 次漏掉整個輪替檔。
   - `head_of(&source)`、`read_lines(&source)` 用路徑重開檔，也有同類問題。
   - 修法：開檔一次，用 `fstat` 確認 inode，開頭與內容都從同一個 handle 讀；宣告遺失前短暫等待後重掃一次；同 inode、同開頭的檔換了名字不算「被換掉」。
   - 測試：要有能穩定重現這個競爭的測試（可以加只在測試啟用的延遲掛鉤）。
2. **缺口回報不冪等、重送沒有退避**（`collect.rs:374`、`:454` 的 `send_gaps`；`store.rs:1663` 的 `record_gap`）
   - 多筆待送缺口中前面成功、後面失敗，或 server 存了但回應逾時，下一輪會全部重送，畫面出現重複的缺口。
   - 修法：每筆被接受就從待送清單移除；server 端加唯一鍵（例如叢集、收集元件、instance、起點、原因，或收集元件產生的缺口 id），用 `INSERT OR IGNORE`。重送加退避。`deploy.md` 寫明升級順序：先升 server，再升收集元件（舊 server 不認得新的來源，會一直回 400）。
3. **安靜時段的缺口起點太早**（`collect.rs:354`）
   - 起點用「最後一筆紀錄的時間」：01:00 最後一筆、一直跟到 05:00 才重啟，會回報 01:00–05:01 沒有收集，實際只少約 1 分鐘。這會讓 Audit Loupe 把自己的失敗講得比實際嚴重，稽核人員會被誤導。
   - 修法：讀到沒有新資料且落後量為 0 時，記一個「已跟上的時間」；起點取 `max(最後一筆時間, 已跟上的時間)`。`started_ms` 要在算完結尾位置之後才取。
4. **用第一筆時間挑下一個檔**（`collect.rs:326-330`）
   - audit log 的 `stageTimestamp` 不是嚴格遞增；下一個輪替檔的第一筆若 ≤ `last_ms`，整個檔被跳過。`last_ms` 是空的時候所有輪替檔都被跳過，而且缺口長度是 0。
   - 修法：依輪替檔名（UTC 時間）或最後一筆時間決定順序；缺口一定不能是 0 長度。
5. **測試寫死日期**（`api.rs:735`、`:742`，配合 `:712` 與 `:171` 的 400 天上限）：約 2027-11 之後會失敗。改成相對於現在的時間。
6. **檔案消失時程式直接結束**（`:393`、`:417`、`:419`、`:425` 的 `?`）：當成遺失或重試處理，不要讓程序結束。
7. **輪替檔最後一行不完整**（`collect.rs:421`）：現在被無聲丟掉，要記成被擱置的行或缺口。
8. **server 端既有 bug，在同一條路上**（`audit.rs:252` 的 `parse_body`）：只有一行的 batch，內容含 `"items"` 字樣，會被當成空的 EventList 丟掉（回 200、received 0）。collect-file 送的 batch 要先當 JSON lines 解析。EventList 裡有一筆壞掉時，其他好的也被丟掉（`audit.rs:272`），一起修。
9. 小項：
   - `record_rejected` 在寫入失敗重試時會重複記（`api.rs:42-48`）。
   - server 沒有限制缺口回報裡 `detail`、`collector`、`instance` 的長度（`api.rs:51`）。
   - `AL_HEARTBEAT_SECS` 超過 180 秒會造成假的心跳缺口：限制上限或寫進文件。
   - chart 註解 `deploy/chart/templates/collectors.yaml:310`（「a restart resumes」）已經不對。
   - `design.md:71` 連到需求文件的原則 8，PR #10 合併前是斷的連結；確認 PR #10 合併後就對了，不用改。

## 3. 不在範圍（寫進 PR 描述的「後續」）

- collect-rancher 重啟後仍會補讀（`design.md:72`），違反原則 8，另開一個 PR。
- 資料段表、遺失前警告、後端停機的本機暫存、進度回報（第 3.6 節 (3) 第 4、5、8 點與第 3.7 節）。
- 版本號：不要改 `Cargo.toml`、`Chart.yaml` 的版本，也不要打 tag；程式註解寫的「0.3.2」改成不寫版號。

## 4. 測試與驗證

- 每一項修正至少一個測試，先在修正前跑出失敗，再跑出通過，兩次結果都放 PR 描述。做不到「先失敗」的要說明原因。
- 既有測試全部通過；`cargo clippy --all-targets` 不能比 main 多出新警告（目前 main 有 19 個既有警告，不用修）。
- 舊資料庫升級：用 `~/audit-loupe-lab/overnight/db/al.db` 的**副本**（不要動原檔）跑一次 migration，確認舊的缺口、新的唯一鍵、被擱置的行都正常。
- 回放測試加一個情境：用 `~/audit-loupe-lab/overnight/raw/` 的真實輪替檔（**複製一份再用**，原檔不動），模擬執行中輪替與停機輪替，筆數要與原始行數對得起來。

## 5. 回報

- 更新 PR #11 的描述：每一項怎麼修、行為改了什麼、測試結果（修正前失敗／修正後通過）、升級注意事項。
- 報告 `docs/lab/report-pr11-followup.md` 放在 PR #11 裡，最上面一張表：項目、狀態、需要 Sam 決定的事。
- 硬性限制照舊：不印 token／密碼／IP、Mac 不用 sudo、不碰叢集、不 force-push（真的需要時先停下來寫進報告）。
