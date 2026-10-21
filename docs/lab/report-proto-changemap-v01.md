# 報告：原型 API v0.1 的正確性（軌 1）

依據：`docs/lab/parallel-1021b-task.md` 的「軌 1」。分支 `proto/changemap`，PR #13。執行時間：2026-10-21 14:37 開始。

**一句話結論**：patch 照步驟套上、測試全過、本機 server 已更新。用真實資料驗證時，翻頁、請求類計數、整批列、detail 都正確；但**真實的 exec 紀錄不會觸發「SUSE Security」那一項**，因為 patch 只認 `verb=create` 的 exec，而這批資料裡的 exec 全都是 `verb=get`。照任務書沒有自己修，寫在「卡住的地方」。

名詞：
- **整批列**：沒有名稱的寫入（例如一次刪掉一整類資源、由系統命名的建立），在畫面上合成一列，名稱顯示為 `*`。
- **請求類資源**：只是詢問、不會留下物件的資源（例如「我有沒有這個權限」），不列出，只計數到 `hidden.requestOnly`。
- **cursor**：翻到下一頁時要帶回去的字串。

## 步驟對照表

照任務書指定的順序：1-1 → 1-2 → 1-3（含 push）→ 1-9 → 1-10 → 1-4 → 1-11。

| 步驟 | 做了什麼 | 證據 | 結果 |
|---|---|---|---|
| 1-1.1 | 確認 PR #12 已合併 | `gh pr view 12 --json state` → `MERGED`（2026-10-21 06:34 UTC） | 完成 |
| 1-1.2 | 確認主工作目錄的變更 | `git status` 只有 `docs/api/changemap-v0.md`（修改）與 `docs/lab/parallel-1021b-task.md`（新檔），沒有別的變更 | 完成 |
| 1-1.3 | `git fetch origin`，`git switch -c docs/decisions-1021b origin/main` | 兩個檔的變更跟著帶過去 | 完成 |
| 1-1.4 | 只 commit 這兩個檔，push，開 PR | commit `4032665`（2 個檔），PR #14，標題照任務書 | 完成 |
| 1-1.5 | 刪掉已合併的 `docs/decisions-1021` | 被 Claude Code 的自動模式擋下（判定為具破壞性的 git 操作），本機與遠端都**還在** | **卡住**，見下面第 1 點 |
| 1-2 | 比對 sha256，`git am` 套用 | sha256 `0c79fb3a…5aa02` 等於 README；套用前分支在 `50ac250`；套用後 commit `bf9a680`，只動 `api.rs`、`changemap.rs` | 完成 |
| 1-3 | `cargo test --locked`、`cargo clippy --all-targets`，push | 測試 104 通過、0 失敗、1 略過，和 README 一致。clippy 主程式 **19**、測試 **21**，README 是 17、19 → 停下來問 Sam；Sam 認同是 clippy 版本差異後才 push | 完成（有差異，見第 2 點） |
| 1-3 CI | 確認 PR #13 的 CI | 見下面「CI 結果」 | 見下方 |
| 1-9 | `cargo build --release --locked`，`deploy-bin.sh` 更新 | `deploy-bin.sh` 印 `both up`；`/api/changemap/detail?key=x` 回 400（新版）；回應有整批列（10/20 第一頁有 2 列）；cursor 是十六進位字串；不是這個 server 發出的 cursor（`100`）回 400；detail API 回 200 | 完成 |
| 1-10 | `AL_DATA_NOTE` | 在 `~/audit-loupe-lab/proto/com.sam.audit-loupe-proto.plist` 加一行；`plutil -lint` OK；照 STATUS.md 用 `start-server.sh` 重新載入；回應多了 `meta.dataNote`（文字照任務書）與 `meta.dataUntilMs`（= 2026-10-21 00:02:22 台北時間） | 完成 |
| 1-4 翻頁 | 10/17–10/21 每天翻完所有頁 | 每天的總列數都等於 `counts.objects`，沒有重複的 key（見下表） | 完成，通過 |
| 1-4 請求類 | `hidden.requestOnly` 對照 sqlite 唯讀查詢 | 5 天都一致（見下表） | 完成，通過 |
| 1-4 整批列 | admin 建立 `users`／`globalrolebindings`／`tokens` 的日子 | 只有 10/17 有；三類的整批列都出現 | 完成，通過 |
| 1-4 detail | 有 exec 的 Pod：detail API 與整頁回應附的 `detail` 相同；`dependsOn` 要有 SUSE Security | 兩者**相同**；但 **`dependsOn` 是空的** | **不通過**，見第 3 點 |
| 1-11 | push、更新 PR #13 描述、本報告 | 本報告 commit 在 `proto/changemap`；PR #13 描述已更新 | 完成 |

## 卡住的地方

1. **刪除舊分支被擋（1-1.5）。** `git branch -D docs/decisions-1021` 與 `git push origin --delete docs/decisions-1021` 被 Claude Code 的自動模式擋下，我沒有改用其他方法繞過。PR #12 已合併，所以分支留著不影響內容。**建議**：Sam 自己執行：
   ```sh
   git -C ~/audit-loupe branch -D docs/decisions-1021
   git -C ~/audit-loupe push origin --delete docs/decisions-1021
   ```
   這個分支的歷史 `f5b0200` 裡有一個真實的 lab IP（見 `report-pr11-followup.md`）。PR #12 若是用 merge commit 合併，刪掉分支後，main 的歷史裡仍然會有那個 commit。

2. **clippy 的數字和 README 不一樣（1-3）。** Mac 上是主程式 19、測試 21（`clippy 0.1.99`，2026-09-28），README 是 17、19。21 個警告都在 `store.rs`、`rulestate.rs`、`access.rs`、`demo.rs`，**沒有一個在 patch 改的 `changemap.rs`、`api.rs`**。依任務書停下來問 Sam；Sam 確認是 clippy 版本不同後才繼續。我沒有在 `50ac250` 重跑來證明，因為那要另開 worktree，任務書沒有這一步。

3. **真實的 exec 不會出現「SUSE Security」那一項（1-4，patch 的問題）。**
   - 現象：10/20 有 exec 的 Pod，`/api/changemap/detail` 和整頁回應附的 `detail` 完全相同，但 `dependsOn` 是 `[]`，操作的 verb 顯示成 `get`，不是 `exec`。
   - 原因：資料庫裡 Pod 的 exec 紀錄有 14 筆，**verb 全部是 `get`，沒有一筆是 `create`**。新版 kubectl 用 websocket 開 exec，以 GET 發起。patch 只在 `verb == "create"` 時把動作改成子資源名稱（`changemap.rs` 的 `let verb = if sub.is_empty() || verb != "create" { verb } else { sub.clone() };`），`dependsOn` 又只看動作是不是 `exec`／`attach`。單元測試 `detail_for_one_object` 用的是 `create`，所以測試會過。
   - 同一個原因也讓這幾列在 detail 裡顯示成 `get`。v0（`50ac250`）就是同樣的寫法，不是 v0.1 才有的；但 v0.1 新增的 `dependsOn` 因此在真實資料上不會出現。
   - 照任務書（「不要自己改程式」）**沒有修**。
   - **建議改法**（給下一個 patch）：子資源是 `exec`／`attach`／`portforward` 時，不論 verb 是 `get` 還是 `create`，都用子資源名稱當動作；單元測試加一筆 `verb=get, subresource=exec` 的紀錄，確認 `dependsOn` 會出現。

## 任務書沒寫但我做了的事

- 1-1.4 commit 前，另外用 grep 掃了這兩個檔有沒有 IP 與 token（只讀，結果是沒有）。原因是上一批曾把真實 IP 推上公開 repo。
- 1-3 數字不一致時，列出每個 clippy 警告的位置來找原因（只讀）。
- 1-4 找出 `dependsOn` 為空的原因時，另外查了資料庫裡 exec 紀錄的 verb（唯讀），也讀了程式碼。

以上都沒有改任何檔案或設定。

## 1-4 驗證結果

方法：`127.0.0.1:18090` 用預設參數（全部叢集、不含系統元件），從第一頁一直帶 `next` 翻到沒有下一頁。sqlite 用 `mode=ro` 開 `~/audit-loupe-lab/proto/db/al-copy.db`。請求類的查詢照約定的定義寫：當天（台北時間）、`name` 為空、資源是 5 種請求類之一、是寫入或有中高風險警示、排除已對到下游紀錄的 Rancher 紀錄。

| 日期 | 頁數 | 總列數 | `counts.objects` | 不重複的 key | 整批列 | `requestOnly`（API） | sqlite 查詢 |
|---|---|---|---|---|---|---|---|
| 10/17 | 2 | 159 | 159 | 159 | 9 | 27 | 27 |
| 10/18 | 2 | 177 | 177 | 177 | 5 | 379 | 379 |
| 10/19 | 3 | 222 | 222 | 222 | 5 | 0 | 0 |
| 10/20 | 3 | 293 | 293 | 293 | 2 | 0 | 0 |
| 10/21 | 1 | 3 | 3 | 3 | 0 | 0 | 0 |

admin 建立 `users`／`globalrolebindings`／`tokens`（只有 10/17）：

| 資源 | admin 的建立（sqlite） | 整批列 | 列上的操作數 | 列上的「誰」 |
|---|---|---|---|---|
| `users` | 2 | `local/management.cattle.io/users//*` | 2 | admin |
| `globalrolebindings` | 2 | `local/management.cattle.io/globalrolebindings//*` | 2 | admin |
| `tokens` | 1 | `local/management.cattle.io/tokens//*` | 2 | 一個 lab 測試帳號 |

`tokens` 那一列有 2 筆操作，因為同一天另一個 lab 測試帳號也建了 token；「誰」取當天最後一個有執行者的操作，所以顯示那個帳號。這和 v0 的規則一樣。

exec 的 Pod（10/20，下游叢集）：

| 檢查 | 結果 |
|---|---|
| `/api/changemap/detail` 與 `/api/changemap?select=` 附的 `detail` 相同 | 相同 |
| 整頁回應的 `detail.key` 就是選的物件 | 是 |
| `dependsOn` 有 SUSE Security 那一項 | **沒有**（`[]`），原因見「卡住的地方」第 3 點 |

## CI 結果

`bf9a680`（patch）在 PR #13 的 CI：`backend tests` 通過（2 分 21 秒）、`helm chart` 通過（20 秒）。這份報告的 commit 只有文件，CI 結果寫在 PR 描述。

## 其他觀察（不影響結果）

- cursor 是把「events 最大 id、versions 最大 id、排序鍵」轉成十六進位的字串。介面要當成不透明字串原樣帶回；它不是加密的，看得出內容，但這個 server 會擋下不是它發出的格式。
- `deploy-bin.sh` 照原本的用法執行，也重新啟動了 18092 的重播 server（腳本本來就會這樣做）。
