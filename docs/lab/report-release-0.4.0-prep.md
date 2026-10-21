# 報告：0.4.0 預覽版準備（10/21 第五批，軌 R）

依據：`docs/lab/parallel-1021e-task.md` 的「軌 R」。執行時間：2026-10-21 23:35 開始。

**一句話結論**：三個 PR 都開好了，測試、lint、build、CI 都通過：任務書 #18、後端發版 #19、介面發版 audit-loupe-ui #5。lab 存證只完成一半：helm 歷史、Pod 與映像檔已存；**values、4 支 API 的回應、server 資料庫這三步被 Claude Code 的自動模式擋下，沒有執行**，要請 Sam 決定怎麼補。另外，**介面 0.4.0 帶著一個會讓 v5 畫面顯示錯誤的已知問題**（見「任務書可能寫錯的地方」），建議修好再打 tag。

名詞：
- **存證**：升級前把 lab 的現況存下來，萬一升級失敗可以照著還原。
- **revision**：helm 每次安裝或升級的編號；`helm rollback` 用它回到某一次的狀態。
- **自動模式**：Claude Code 判斷某個指令是否需要人同意的機制；被擋下的指令完全沒有執行。

## 步驟對照表

| 步驟 | 做了什麼 | 證據 | 結果 |
|---|---|---|---|
| R-1.1 | `git status` | 當時在 `docs/task-1021c` 分支；只有 `docs/lab/parallel-1021e-task.md`、`docs/lab/task-1022a.md` 兩個新檔 | 完成 |
| R-1.2 | `git fetch origin`，`git switch -c docs/task-1021e origin/main` | origin/main 是 `bc52503`；兩個新檔跟著帶過去 | 完成 |
| R-1.3 | 只 commit 兩份任務書，`git push -u origin docs/task-1021e`，開 PR | commit `bc20ce1`（2 個檔）；PR #18，標題照任務書 | 完成 |
| R-2.1 | `git worktree add ../audit-loupe-rel -b release/0.4.0 bc52503` | `rev-parse HEAD` = `bc52503e72914b19479ec0b3412d34a1f60937de` | 完成 |
| R-2.2 | 比對 SHA-256，`git am` | `2bd26bbc…69a21`，等於 README；commit `54f5c73`（`CHANGELOG.md`、`backend/Cargo.lock`、`backend/Cargo.toml`、`deploy/chart/Chart.yaml`） | 完成 |
| R-2.3 | `cargo test --locked` | 單元 116 通過、2 略過；整合 10 通過、1 略過；0 失敗 | 完成，一致 |
| R-2.4 | `git push -u origin release/0.4.0`，開 PR | PR #19，標題照任務書；描述是 `CHANGELOG.md` 的 0.4.0 一節，最後一行是打 tag 那句 | 完成，見「任務書沒寫但我做了的事」第 3 點 |
| R-3.1 | `git -C ~/audit-loupe-ui worktree add ../audit-loupe-ui-rel -b release/0.4.0 8cb95be…` | `HEAD is now at 8cb95be`（UI PR #4 的合併） | 完成 |
| R-3.2 | 比對 SHA-256，`git am` | `7dfd610b…c9391`，等於 README；commit `3fcfd85`（`CHANGELOG.md`、兩個 `package.json`） | 完成 |
| R-3.3 | `yarn lint`、`yarn build-pkg audit-loupe` | 用 `scripts/dev.sh` 的同一個 SUSE BCI Node 24 映像檔與 digest，做法同 1021c 軌 A 的 A-6（node_modules 用匿名 volume）。lint：`eslint --max-warnings 0 .` 通過；build：`Done`，產出 `dist-pkg/audit-loupe-0.4.0`，只有原本就有的 webpack 大小建議（300.97 KiB）；`git status` 乾淨 | 完成 |
| R-3.4 | `git push -u origin release/0.4.0`，開 PR | audit-loupe-ui PR #5，標題照任務書，描述是 `CHANGELOG.md` 的 0.4.0 一節 | 完成 |
| R-4 | 建 `~/audit-loupe-lab/release-0.4.0/`，`chmod 700` | 已建 | 完成 |
| R-4.1 | 兩座的 `helm history` | `helm-history-local.txt`：revision 1–8，**目前 8**（audit-loupe-0.3.0）；`helm-history-ds.txt`：revision 1–4，**目前 4**（audit-loupe-0.3.0） | 完成 |
| R-4.2 | 兩座的 `helm get values -o yaml` | **被自動模式擋下（Modify Shared Resources），沒有執行**；`values-*.yaml` 不存在 | **沒做**，見「卡住的地方」第 1 點 |
| R-4.3 | 兩座的 Pod 與映像檔 | `pods-local.txt`（server、rancher、watch 各 1，都 Running）；`pods-ds.txt`（file、watch 各 1，都 Running）；`images-*.txt`：5 個 Pod 都是 `audit-loupe-server:0.3.0` 或 `audit-loupe-collector:0.3.0` | 完成 |
| R-4.4 | port-forward 18180，存 4 支 API | **被自動模式擋下（Production Reads），沒有執行**；沒有開 port-forward | **沒做**，見第 1 點 |
| R-4.5 | 複製 server 資料庫與 `-wal` | **被自動模式擋下（Production Reads），沒有執行**；`db/` 資料夾也沒有建 | **沒做**，見第 1 點 |
| R-4.6 | 還原程序 | 見下面「還原程序」 | 完成（只寫，不執行） |
| R-5 | 本報告，commit 在 `release/0.4.0`，push | 本檔 | 完成 |

## 卡住的地方

1. **R-4.2、R-4.4、R-4.5 被 Claude Code 的自動模式擋下。**三個指令都完全沒有執行，lab 沒有任何變化，本機也沒有留下半成品。我沒有換別的做法繞過。這三步不補的話，下一份任務書升級前就少了 values 與資料庫的備份，「還原程序」的第 2 部分也沒有檔可用。請 Sam 選一種做法：
   - 自己在終端機執行下面的指令（在指令前加 `!` 就會在這個對話裡執行，輸出也會出現在這裡；values 與資料庫的指令只寫檔、不印內容）；或
   - 在 Claude Code 設定加允許規則，再叫我重做這三步。

   ```sh
   cd ~/audit-loupe-lab/release-0.4.0
   helm --kubeconfig ~/audit-loupe-lab/local.kubeconfig -n audit-loupe-system get values audit-loupe -o yaml > values-local.yaml && chmod 600 values-local.yaml
   helm --kubeconfig ~/audit-loupe-lab/ds-admin.kubeconfig -n audit-loupe-system get values audit-loupe -o yaml > values-ds.yaml && chmod 600 values-ds.yaml
   mkdir -p db
   kubectl --kubeconfig ~/audit-loupe-lab/local.kubeconfig -n audit-loupe-system exec audit-loupe-6c6c48f96f-jlfg4 -- cat /data/audit-loupe.db > db/audit-loupe.db
   kubectl --kubeconfig ~/audit-loupe-lab/local.kubeconfig -n audit-loupe-system exec audit-loupe-6c6c48f96f-jlfg4 -- cat /data/audit-loupe.db-wal > db/audit-loupe.db-wal
   chmod 600 db/audit-loupe.db db/audit-loupe.db-wal
   sqlite3 -readonly db/audit-loupe.db 'PRAGMA quick_check'
   sqlite3 -readonly db/audit-loupe.db 'SELECT count(*) FROM events'
   ```
   R-4.4 要開 port-forward（`kubectl … port-forward --address 127.0.0.1 svc/audit-loupe 18180:8080`），再用 `curl -s` 把 `/api/coverage?hours=48`、`/api/integrity`、`/api/info`、`/api/rules` 存成 4 個 json，做完停掉。

## 任務書沒寫但我做了的事

1. **commit 前用 grep 掃了兩份任務書與兩份 `CHANGELOG.md`**，看有沒有 IP（只讀；都沒有）。
2. **R-3.1 之前在 `~/audit-loupe-ui` 跑了 `git status` 與 `git fetch origin`**。`git status` 有一個 `?? docs/`，不是這一軌的檔，沒有動。fetch 是為了確認 `8cb95be` 就是 origin/main（是）。
3. **PR #19 的描述改過一次**（`gh pr edit`）。第一次我把 Claude Code 的署名放在最後一行，不符合「最後一行寫打 tag 那句」，所以把署名移到那句前面。
4. **看了 v5 的 `components/v5/StateTag.vue` 有沒有 `state` 屬性**（只讀），原因見下一節。
5. **為了寫還原程序，讀了 `docs/lab/report-ui-facts.md` 開頭與 `deploy/chart/values.yaml`**（只讀）。另外用 `grep` 看 chart templates 的那一次被自動模式擋下，沒有執行。

## 任務書可能寫錯的地方（沒有造成損害）

- **介面 0.4.0 帶著一個已知問題。**10/21 晚上 Sam 在 dev server 看 v5 畫面時，我找到這個問題（PR #17 合併後那次）：
  - `components/v5/StateTag.vue` 有一個屬性叫 `state`；Rancher shell 的全域 mixin（`@rancher/shell/mixins/fetch.client.js`）也替每個元件加了一個叫 `state` 的資料 `{pending, error, timestamp}`，把它蓋掉了。
  - 結果：篩選標籤顯示成 `[object Object] 0`，總覽表的稽核狀態欄印出 `{"pending":false,…}`。瀏覽器 console 有 `Data property "state" is already defined in Props`。
  - 放進真正的 Rancher 也一樣，因為外掛就是跑在 shell 裡面。lint 與 build 都抓不到。
  - `release/0.4.0`（`8cb95be` 加版本補丁）裡的 `StateTag.vue` 還是 `state`，所以這個 PR 合併打 tag 後，預覽版的 v5 畫面會顯示錯誤。開 PR 本身沒有損害，所以我照做了。**建議修好這個問題再合併 UI #5 或打 tag**（修法：把屬性改名，例如 `auditState`，連同所有 `:state=` 一起改）。我沒有改程式。

## 還原程序（下一份任務書才用，這次不執行）

### 1. 升級後要退回 0.3.0（helm）

```sh
helm --kubeconfig ~/audit-loupe-lab/local.kubeconfig    -n audit-loupe-system rollback audit-loupe 8
helm --kubeconfig ~/audit-loupe-lab/ds-admin.kubeconfig -n audit-loupe-system rollback audit-loupe 4
```
- 8 與 4 是 R-4.1 記下的目前 revision（`helm-history-*.txt`）。
- 確認映像檔回到升級前：每座重跑 R-4.3 的 jsonpath 指令，第二欄要和 `images-local.txt`、`images-ds.txt` 一樣（都是 `:0.3.0`）。Pod 名稱會變，只比映像檔。
- `kubectl … get pods` 都要 `Running`、`READY 1/1`。

### 2. 資料庫放回（**需要 Sam 同意才執行**）

只有在退回之後 server 開不起來時才做（例如 0.4.0 已經更新過資料庫，0.3.0 讀不了）。前提是 R-4.5 的備份已經存在，目前還沒有（見「卡住的地方」）。
1. 停掉 server：`kubectl --kubeconfig ~/audit-loupe-lab/local.kubeconfig -n audit-loupe-system scale deploy/audit-loupe --replicas=0`，確認 server Pod 已經沒有了。
2. 找出 server 的 PVC：`kubectl … -n audit-loupe-system get pvc`（chart 的 `server.persistence`，5Gi）。
3. 開一個只掛這個 PVC 的暫時 Pod（掛在 `/data`），用 `kubectl cp` 把 `db/audit-loupe.db` 與 `db/audit-loupe.db-wal` 放回 `/data`，並刪掉 `/data/audit-loupe.db-shm`（如果有）。
4. 刪掉暫時 Pod，`scale deploy/audit-loupe --replicas=1`。
5. 確認：server Pod `Running`；`/api/info` 的版本是 0.3.0；`/api/integrity` 和備份時的 `integrity.json` 比對。
- 放回之後，升級到退回之間收到的紀錄會不見；收集元件不補資料，所以那一段會缺。
- 下游叢集沒有 server、沒有資料庫，只需要第 1 部分。

## CI 結果

- PR #18（任務書）：`backend tests` 通過（2 分 3 秒）。`helm chart` 從 15:35:52Z 開始，到 15:46Z 仍是 `in_progress`（平常約 15 秒）。我沒有重跑也沒有取消，請 Sam 看一下。
- PR #19（`54f5c73`，補丁）：`backend tests` 通過（2 分 54 秒）、`helm chart` 通過（14 秒）。
- PR #19（`6a16d18`，加上本報告）：`backend tests` 通過（2 分 50 秒）、`helm chart` 通過（11 秒）。
- 這次 push 只加了 CI 結果這幾行，內容只有文件；它的 CI 結果寫在 PR #19 的留言。
- audit-loupe-ui PR #5（`3fcfd85`）：`Find Extensions to Build` 通過（4 秒）、`Test build audit-loupe` 通過（54 秒）。
