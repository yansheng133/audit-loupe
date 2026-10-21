# 回報：PR #11 合併前的補強

執行：2026-10-21 09:13–09:40（台北）。對應：`docs/lab/pr11-followup-task.md`（`parallel-1021-task.md` 的軌 A）。沒有碰任何叢集，沒有合併、沒有發版，PR #11 的分支沒有 force-push。

## 一句話結論

第 1 節三個決定、第 2 節第 1–8 項都做完，第 9 項做了 4 個小項（第 5 個不用改），每一項都有測試。最大的改變是：**正在讀的檔一直開著**，認檔、讀開頭、讀內容都用同一個開啟的檔。所以輪替瞬間的認檔競爭沒有了，正在讀的檔被刪掉也照樣讀得完。

## 總表

| 項目 | 狀態 | 需要 Sam 決定的事 |
|---|---|---|
| 0 未 commit 的文件 | 完成：PR #10 已合併，另開 PR [#12](https://github.com/yansheng133/audit-loupe/pull/12)（`docs/decisions-1021`） | **#12 的分支歷史有一個真實的 lab IP**（見「事故」）。要不要請 GitHub 清快取，或接受 squash merge 後刪分支就好 |
| 1-1 首次安裝讀不讀現有 `audit.log` | 完成：`collector.file.onFirstStart`，預設 `fromNow` | 「安裝前既有」用一筆「開始收集」標記的時間範圍來表示，沒有在每一筆紀錄上加旗標（見下）。這樣夠不夠 |
| 1-1 狀態檔遺失不等於首次安裝 | 完成：先問 server（新的 `GET /ingest/v1/known`） | — |
| 1-2 心跳缺口與回報缺口都留 | 完成：`/api/coverage` 每筆缺口加 `kind`（`heartbeat`／`reported`） | — |
| 1-3 被擱置的行查得到 | 完成：`rejectedCounts` 與新的 `GET /api/rejected` | — |
| 2-1 輪替瞬間的認檔競爭 | 完成（改成開著檔讀） | — |
| 2-2 缺口回報不冪等、沒有退避 | 完成 | — |
| 2-3 安靜時段的缺口起點太早 | 完成；最多高估約 30 秒 | 30 秒可不可以接受（為了不每 2 秒寫一次狀態檔） |
| 2-4 用第一筆時間挑下一個檔 | 完成 | — |
| 2-5 測試寫死日期 | 完成 | — |
| 2-6 檔案消失時程式結束 | 完成（迴圈裡的 `?` 都改掉了） | — |
| 2-7 輪替檔最後一行不完整 | 完成：送給 server 擱置 | — |
| 2-8 `parse_body` 的 `"items"` 問題 | 完成 | — |
| 2-9 小項 | 4 項完成；`design.md:71` 的連結確認 PR #10 合併後就對了，沒改 | — |
| 4 測試與驗證 | 完成，見下 | — |

## 每一項怎麼修、行為改了什麼

### 第 1 節：Sam 的三個決定

1. **首次安裝**
   - 新設定 `collector.file.onFirstStart`，對應環境變數 `AL_ON_FIRST_START`。
   - `fromNow`（預設）：從 `audit.log` 結尾開始讀，並送一筆 `reason: collection-start`、`detail: fromNow` 的標記。`/api/coverage` 把它放在新的 `starts` 陣列，**不算缺口**。
   - `readExisting`：從頭讀目前的 `audit.log`，輪替出去的舊檔不讀。標記的 `detail` 是 `readExisting`，`fromMs` 是檔案第一筆紀錄的時間，`toMs` 是啟動時間。
   - **設計選擇**：「安裝前既有」不是在每筆紀錄上加旗標，而是用這個時間範圍表示。這個節點在這段時間的紀錄，都是安裝時才讀進來的。這樣 events 表與雜湊鏈都不用改。畫面要逐筆標示時，用節點加時間範圍就能判斷。
   - `values.yaml` 的註解與 `docs/deploy.md` 都寫了開啟的三個影響：不是即時收的、舊檔不讀所以可能不完整、第一次最多約 100 MB。
2. **狀態檔遺失**
   - 收集元件沒有任何狀態時，先問 server 的 `GET /ingest/v1/known?collector=file&instance=<節點>`，用的是同一個收集元件 token，叢集由 token 決定。
   - server 查這個節點有沒有心跳紀錄或缺口紀錄（被別的 instance 取代時，心跳列會刪掉，但會留下缺口列）。
   - 有的話當成重新啟動：從現在開始讀，從 server 最後一次收到這個節點心跳的時間到現在，記一筆 `collector-state-lost`。
   - **一定要在第一次心跳之前問**，不然問的時候 server 已經認得這個節點了。所以程式裡把心跳的啟動移到判斷之後。
   - server 連不上時，每 5 秒重試，不會自己猜。
   - `deploy.md` 第 10 節「可以刪這個目錄」那句已補上這個影響。
3. **兩種缺口都留**
   - 心跳缺口與收集元件回報的缺口原本就分開存。這次 `/api/coverage` 的每筆缺口加上 `kind`：`heartbeat` 或 `reported`。同一段時間兩種都會出現，不合併。
   - 測試 `a_resent_gap_report_is_kept_once_and_both_kinds_are_returned` 驗證重疊時兩筆都在。
4. **被擱置的行**
   - collect-file 每批都帶 `x-audit-loupe-instance`、`x-audit-loupe-file`、`x-audit-loupe-offset`（這批從檔案哪個位置開始）。server 用它們加上該行在這批裡的位置，算出那一行在檔案裡的確切位置。
   - `rejected_lines` 加 `instance`、`file`、`offset` 三欄。
   - `/api/coverage` 加 `rejectedCounts`（依來源、節點）。
   - 新的 `GET /api/rejected?from&to|hours&cluster&limit` 回傳時間、叢集、節點、原因、檔名、位置、長度、sha256，**不回傳原文**，測試會檢查原文沒有出現。

### 第 2 節：審查發現

1. **認檔競爭**
   - 正在讀的檔一直開著（`cur: Option<(PathBuf, File)>`）。inode 用 `fstat`，開頭雜湊與內容都從同一個 handle 讀，每一輪都重新比對開頭。
   - 只有在需要重新找檔時（啟動、讀取錯誤）才列目錄。找不到時等 300 毫秒再找一次，才宣告遺失。
   - 同 inode、同開頭的檔換了名字，本來就算同一個檔。
   - 附帶好處：正在讀的檔就算被 `maxbackup` 刪掉，也讀得完，不會漏。
2. **缺口回報冪等與退避**
   - 收集元件：每筆被 server 接受就立刻從待送清單移除。失敗時退避，1 秒起加倍，上限 300 秒。
   - server：在 `(cluster, collector, instance, from_ms, reason)` 上建唯一索引，只涵蓋 `reason <> ''` 的列，所以心跳缺口不受影響。寫入改用 `INSERT OR IGNORE`。
   - migration 建索引前，先刪掉已經重複的列（保留最早的一筆）。
   - `deploy.md` 寫了升級順序：先升 server，再升收集元件。
3. **安靜時段的起點**
   - 新增 `caught_up_ms`：讀到 `audit.log` 沒有新資料時，記下現在時間。最多每 30 秒寫一次狀態檔，所以最多高估約 30 秒。
   - 缺口起點取 `max(最後一筆, 已跟上的時間)`。啟動時間在算完結尾位置之後才取。
4. **挑下一個檔**
   - 一般情況照輪替檔名（UTC 時間）排序，取下一個。
   - 正在讀的檔已經不在目錄時，取檔名比它新的最舊一個。所以收集元件會一直記住目前這個檔最後被看到的名字（新欄位 `name`）。
   - 不知道名字時，用每個檔的**最後一筆**時間判斷，不再用第一筆。
   - 回報的缺口一定不是 0 長度（`to = max(to, from + 1)`）。
   - **新偵測**：還沒開始讀的輪替檔先被刪掉時，下一個還在的檔的第一筆，會比目前這個檔輪替的時間晚超過 5 秒（kube-apiserver 在「寫進新檔的那一筆」時輪替）。這時記一筆 `file-removed`，範圍是輪替時間到下一個檔的第一筆。
5. **測試寫死日期**：改成相對於現在的時間。
6. **檔案消失時結束**
   - 迴圈裡的 `?` 都改成記警告後重試或當成遺失。狀態檔寫入失敗也只記警告。
   - 啟動階段（還沒進迴圈）的 `?` 保留，失敗時讓 Pod 重啟。
7. **輪替檔最後一行不完整**：把剩下的位元組加上換行送給 server，server 擱置並記下位置。
8. **`parse_body`**
   - 新的 `parse_body_lenient`：只有最上層 `kind` 是 `EventList` 時才當清單處理，其他一律當 JSON lines。
   - EventList 裡壞掉的那一筆單獨擱置，其他照存。
   - 原本的 `parse_body` 只剩測試在用，所以改成只在測試時編譯。
9. **小項**
   - `record_rejected` 重送時重複記：以 `(cluster, instance, file, offset, sha256)` 為唯一鍵，用 `INSERT OR IGNORE`。
   - `collector`、`instance`、`detail` 的長度上限：64／256／1024 位元組，超過回 400。header 也截到 256 字元。
   - `AL_HEARTBEAT_SECS` 上限改成 120 秒。
   - chart 註解（`collectors.yaml` 的 state volume）已更正。
   - 程式註解裡的「0.3.2」已拿掉。

## 測試結果

- 修正前，指的是 PR #11 原本的 commit `32df4e8`，在 scratchpad 的副本上跑同一份新測試檔；修正後是這次的 commit。

### 整合測試（真的執行收集元件，接假的 ingest server）

| 測試 | 修正前 | 修正後 |
|---|---|---|
| `first_start_reads_from_now_and_marks_the_start` | 失敗（從頭讀了既有的行） | 通過 |
| `first_start_can_read_the_existing_file` | 失敗（沒有開始標記） | 通過 |
| `lost_state_is_a_restart_when_the_server_knows_the_node` | 失敗（當成首次安裝從頭讀） | 通過 |
| `rotation_during_a_slow_listing_loses_nothing`（列目錄時加 150 毫秒延遲） | 失敗 3/3：1,810 行收到 1,509 行 | 通過 5/5 |
| `quiet_cluster_restart_does_not_overstate_the_downtime` | 失敗（起點是 4 小時前） | 通過 |
| `removed_file_is_still_read_through_the_open_handle` | 失敗（B 漏掉、記了缺口） | 通過 |
| `files_removed_before_they_were_read_are_reported` | 失敗 | 通過 |
| `lag_counts_rotated_files_and_live_rotation_loses_nothing` | 失敗（落後量只算到目前的檔，原因沒查） | 通過 |
| `restart_reads_from_now_and_reports_the_downtime`（已改：起點可以晚於最後一筆） | 通過 | 通過 |
| `same_inode_with_other_content_is_a_new_file`（同上） | 通過 | 通過 |

- **競爭測試的比較方式**：修正前的程式沒有延遲掛鉤，直接跑會通過，因為根本沒有重現到競爭。所以我在 scratchpad 的副本裡加上同一個掛鉤（`AL_TEST_LIST_DELAY_MS`）再跑，結果是上表的 3/3 失敗。這個掛鉤在正式程式裡只有設了環境變數才會生效。
- 整合測試整組連跑 5 次都通過，每次約 18 秒。
- 中途有一次不穩定的失敗，原因是測試的檔名精度：兩次輪替在同一毫秒內時，輪替檔名相同，第二次就把第一個蓋掉了。測試裡已經讓檔名一定不重複，不是程式的問題。

### 單元測試

新增 4 個：
- `a_resent_gap_report_is_kept_once_and_both_kinds_are_returned`
- `set_aside_lines_are_listed_without_text_and_once`
- `a_one_line_batch_mentioning_items_is_not_an_empty_list`
- `set_aside_lines_are_listed_with_where_they_were_read`

它們用到的函式在修正前不存在，所以修正前是**編譯失敗**，不是執行失敗。「先失敗」在這裡只能這樣證明。

### 全部測試

- 單元測試 100 個通過，2 個預設不跑（ignored）：一個是原本就有的，另一個是這次新增的「舊資料庫升級」測試，要指定副本才會跑。
- 整合測試 10 個通過，1 個預設不跑：這次新增的「真實資料回放」測試，同樣要指定副本才會跑。

### 其他檢查

- **clippy**：`cargo clippy --all-targets` 的 warning 與修正前相同，沒有新增。中途多出的兩個（太複雜的型別、`- 0`）已經修掉。
- **舊資料庫升級**：
  - 用 `sqlite3 -readonly … .backup` 把 `overnight/db/al.db` 複製到 scratchpad，再跑 `migrates_a_copy_of_a_real_database`。
  - 結果：16 筆舊缺口都保留（全是心跳缺口）；唯一鍵建立成功並生效；`rejected_lines` 新增 3 欄；重送不重複；第二次開啟不再變動。
  - 原檔 sha256 前後一樣（`164e72db…`）。
- **真實資料回放**：
  - 把 `audit-2026-10-20T08-11-40.325.log` 的前 3 萬行複製到 scratchpad，再跑 `real_audit_lines_replay_with_rotations`，原檔沒動。
  - 執行中輪替：寫 15,000 行，送 15,000 行，audit ID 15,000 個全到，沒有缺口。
  - 停機時輪替：停機期間寫的 10,000 行一行都沒補讀；重啟後的 5,000 行全到；記了一筆 `collector-restart`。

## 升級注意事項

- **先升 server，再升收集元件**。舊 server 不認得 `gap` 來源與 `/ingest/v1/known`：
  - 收集元件第一次啟動（沒有狀態）時會一直重試，等 server 升級。
  - 已經有狀態的收集元件，只是缺口一直送不出去，會退避重試。
- 資料庫 migration 是自動的：`gaps` 先去掉重複再建唯一索引；`rejected_lines` 加 3 欄。
- 行為改變：
  - 預設 `fromNow`。全新安裝不再從頭讀 `audit.log`，之前是從頭讀。
  - 缺口起點可能比最後一筆紀錄晚，最多晚到停機前約 30 秒。
  - `AL_HEARTBEAT_SECS` 超過 120 會被壓到 120。

## 不在範圍（後續）

- collect-rancher 重啟後仍會補讀（`design.md:72`），違反原則 8，另開 PR。
- 資料段表、遺失前警告、後端停機的本機暫存、進度回報。
- 畫面：`starts`、`kind`、`rejectedCounts`、`/api/rejected` 都還沒有畫面，之後在 v5 做。

## 事故

- **PR #12 推了一個真實的 lab IP**：
  - `docs/lab/batch-1021-task.md` 第 116 行原本寫著 lab Rancher 的網址，裡面有真實的私有 IP。
  - 我把掃描和 commit 寫在同一個指令裡，掃描找到了卻沒有擋下 commit，就推上了公開 repo 的 `docs/decisions-1021`（commit `f5b0200`）。
  - 發現後立刻補一個 commit（`4cb27e4`）換成 RFC 5737 的範例位址。依規則沒有 force-push，所以舊 commit 還在這個分支的歷史裡。
  - 建議 squash merge #12 後刪掉分支。要更徹底，需要 Sam 決定是否改寫歷史，或請 GitHub 清快取。
  - 這是私有網段位址，外部連不到，但違反「不在公開內容放真實 IP」的規則。
- 另外兩個我自己的錯，不影響結果：
  - 一開始 PATH 裡沒有 `cargo`，前兩次「編譯沒有輸出」其實沒有執行，我當成編譯通過了。發現後改用 `/opt/homebrew/opt/rustup/bin`，之後的結果都是真的跑過的。
  - 有一次在指令裡多寫了一個會等輸入的 `cat`，指令卡住，已經停掉。

## 用過、留下的東西

- 修正前比對用的 worktree 與建置目錄都放在 scratchpad（不進 repo），做完已刪除。
- 資料庫副本與 3 萬行 log 副本（權限 600）也在 scratchpad，裡面有稽核資料，測完已刪除。`~/audit-loupe-lab/overnight/` 的原檔沒有動。
- `~/audit-loupe-pr11` 這個 worktree 留著，PR #11 合併後可以 `git worktree remove`。
