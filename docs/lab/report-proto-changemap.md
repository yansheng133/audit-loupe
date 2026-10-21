# 報告：軌 B——重播工具、`/api/changemap` v0、本機 server

日期：2026-10-21。任務書：`docs/lab/parallel-1021-task.md` 的「軌 B」。
**一句話結論：五項都完成，含原本先跳過的時間平移。本機 server 以常駐服務在 `127.0.0.1:18090` 跑著 10/17–10/21 的真實資料（al.db 副本），`/api/changemap` 熱的開頁約 40–90 毫秒、冷的約 0.2–0.5 秒；範例檔 15 份已給軌 C。**

沒有碰任何叢集；原始資料只讀不寫；沒有合併、沒有打 tag、沒有推映像檔。

## 1. 做了什麼

| 項目 | 結果 | 位置 |
|---|---|---|
| 重播工具 | 完成，含叢集名稱、不等待、時間平移三個選項。新的子指令 `audit-loupe replay`，走正式的 `/ingest/v1/batch`（同一套 token 驗證、gzip、來源標頭） | `backend/src/replay.rs` |
| 用重播灌本機資料庫 | 完成。下游 4 個原始 log＋10/18 錄影的下游與 Rancher log，共 253,041 筆 | `~/audit-loupe-lab/proto/db/replay.db` |
| 另一個資料來源 | al.db 副本，**選它給 Sam 看**（理由見第 3 節） | `~/audit-loupe-lab/proto/db/al-copy.db` |
| `/api/changemap` v0 | 完成，三個分頁都有；直接查現有資料表，沒有彙總表 | `backend/src/changemap.rs` |
| 測試 | 新增 7 個（changemap 4、replay 3）；全部 99 個通過；新程式沒有 clippy 警告 | `cargo test --locked` |
| 本機 server 常駐 | `127.0.0.1:18090`，launchd 使用者層級服務（登入就啟動、行程結束自動重起，不用 sudo） | `~/audit-loupe-lab/proto/STATUS.md` |
| 範例檔 | 10/17–10/21 × overview／events／relations，15 份 | `~/audit-loupe-lab/proto/fixture-*.json` |

## 2. 重播工具

```sh
audit-loupe replay --url http://127.0.0.1:18093 --tokens <server 的 tokens 檔> --cluster <叢集 ID> \
  --source k8s-audit|rancher-audit [--no-wait] [--speed 600] 檔案...
```

- 叢集名稱：從 server 的 tokens 檔找出該叢集的 token（不印出），server 依 token 決定紀錄屬於哪座叢集，和正式收集元件一樣。
- 不等待：`--no-wait` 一口氣送完；不加時依紀錄間隔除以 `--speed` 暫停（每次最多 5 秒）。
- 時間平移：`--shift 3d`（也接受 `90s`、`30m`、`-2h`）或 `--shift-to <RFC 3339 時間>`（第一筆落在這個時間）。Kubernetes 與 Rancher 紀錄裡的四個時間欄位都會移動。auditID 會加上 `-shift<秒數>`，所以同一份 log 平移後可以和原本的並存，不會被當成重複紀錄。
- 用真實資料驗證過照原始間隔重播與時間平移：
  - 10/18 的 Rancher 紀錄：`--speed 3600 --shift-to 2026-10-21T01:00:00Z`，約 5 小時的紀錄花 9 秒送完，第一筆落在 01:00:00。
  - 10/18 晚上的下游紀錄：`--speed 3600 --shift 3d`，24,560 個不重複的 auditID 全部存進去，每一筆都剛好比原本那筆晚 3 天。
- 驗證：重播進去的下游 apiserver 紀錄，逐日筆數和 al.db 完全相同（10/17 21,928、10/18 71,239、10/19 54,355、10/20 64,913），重複送的部分被 `UNIQUE(cluster, audit_id)` 擋掉。

## 3. 為什麼 Sam 看的是 al.db 副本

重播只能還原原始 log 有的東西：

- **沒有物件版本**：「改了什麼」「找不到執行者的變更」都來自收集元件的物件監看，原始 audit log 沒有。少了它就沒有 `unclear` 狀態，也沒有 local 叢集的大部分內容。
- **Rancher audit 只有 10/18 錄到的那一段**。
- **沒有收集中斷**（重播不送 heartbeat）。

## 4. `/api/changemap` v0 的實作方式

- 雜湊鏈檢查（`detail.hashChain`）不在請求裡做：server 啟動時就在背景用另一條唯讀連線開始檢查，結果用 10 分鐘，過期了在背景重做。第一次檢查完成之前回 `pending`。第一版把檢查放在請求裡，冷開頁要 1.1 秒；改到背景但共用同一條連線時，請求要排隊等檢查做完，反而變成 2.1 秒；最後才改成用另一條連線。
- 一次請求：讀當天的寫入類紀錄（apiserver 的 create／update／patch／delete／deletecollection 與 exec／attach／portforward；Rancher 除 get／list 以外）＋當天有中、高風險警示的紀錄＋當天找不到執行者的物件版本，在記憶體裡依物件鍵分組、判狀態、排序、分頁。
- 狀態照約定的順序判：`look` → `unclear` → `blind` → `clear`。
- `unclear` 的原因依該叢集有沒有 `file` 收集元件（heartbeats 表）決定：沒有就寫「這座叢集沒有收 apiserver audit log」，有就寫「重新連線後比對物件版本才發現」。不是寫死的。
- 身分分類照 `report-ui-facts.md` 第 B 節：`cattle-*` namespace 的 SA → `rancher`；`kube-system` 的 SA 與 `system:` 開頭的固定身分 → `component`；其他 SA → `serviceaccount`；`system:admin` → `human`（「管理者憑證，對不到個人」）；`system:cattle:error` → `unknown`；User 的 principalIds 有 `system://` → `rancher`；只有 `local://` 的人 → `human`（「Rancher 本機帳號，可能多人共用」）。白名單是程式裡的常數。
- `gaps` 只有 `heartbeat`（PR #11 未合併）。

### 和約定不一樣或約定沒寫的地方（請 Sam 決定要不要改約定）

1. `cursor` 是字串化的位移（`"100"`）。
2. `detail` 三個分頁都回，因為右側細節在每個分頁都會顯示。
3. `relations` 的物件節點只放非 `clear` 的前 50 個。
4. `who` 取當天最後一筆**有執行者**的操作。
5. 「Rancher 介面維護動作」的判斷是名稱前綴（`pod-impersonation-shell-`、`pod-impersonation-helm-`、`impersonation-shell-`）與 `userpreferences`／`userattributes`。10/20 擋掉 246 筆。
6. 「系統元件」＝ `component`＋`rancher`。`serviceaccount` 預設顯示（例如 al-sample 的 ci-runner）。
7. `hashChain` 的檢查結果用 10 分鐘；server 剛啟動、第一次檢查還沒做完時回 `pending`。
8. 時間列格子：格內有操作時取最嚴重的狀態；沒有操作但落在缺口內為 `blind`；晚於最後一筆紀錄為 `unknown`。

## 5. 開頁時間（只當參考）

Mac（Apple Silicon）、release build、10/20、`tab=overview`、全部叢集、不含系統元件。冷＝每次重新啟動 server 後的第一個請求。

| 版本 | 1 | 2 | 3 | 4 | 5 | 量的時候的機器負載 |
|---|---|---|---|---|---|---|
| 最後一版，冷（毫秒） | 469 | 278 | 202 | 455 | 192 | 約 9–12 |
| 最後一版，熱（毫秒） | 95 | 74 | 89 | 72 | 56 | 約 7–12 |
| 第一版，冷（毫秒） | 1,124 | 1,115 | 1,127 | 1,211 | 1,120 | 沒記錄 |
| 第一版，熱（毫秒） | 41 | 39 | 39 | 40 | 41 | 沒記錄 |

- 最後一版量的時候，另外兩軌同時在編譯，機器負載約 9–12，所以熱的數字比第一版高。查詢方式兩版相同，熱的差距應該是負載造成的，但沒有在相同負載下重量。
- 最後一版冷的時候，雜湊鏈檢查正在背景跑，和請求搶 CPU；回應裡的 `hashChain` 是 `pending`。
- 第一版 10/18（最多的一天，72,244 筆）熱的約 42 毫秒。
- 一天約 7 萬筆時，掃描當天紀錄、在記憶體分組還很快；10 倍（約 72 萬筆）沒有量，任務書也沒有要求。

## 6. 限制與要注意的

任務書交付的項目都已完成。以下是 v0 的限制，或要等其他事情：

- **`events` 的狀態轉變是近似**：用第一次出現警示、第一次出現找不到執行者代替。約定允許這樣做，每筆的 `reason` 都有寫明是近似。
- **`gaps` 只有 `heartbeat` 一種**：`reported` 要等 PR #11 合併，約定也寫明 v0 可以只回 `heartbeat`。
- **Pod 與自訂資源的 `dependsOn` 是空的**：既有的物件關係表只記 Deployment、RoleBinding、Service 等，不記 Pod 與自訂資源。有記的種類都有回傳，例如 Deployment 會列出掛載的 ConfigMap 和使用的 ServiceAccount。
- **第 4 節列的 8 點和約定不一樣**，等 Sam 決定要不要改約定。
- **約定文件 `docs/api/changemap-v0.md` 不在這個分支**：它在 local main 的 f5b0200，照分工由軌 A 收進 PR。
- **範例檔含真實的身分與物件名稱**，只放在 `~/audit-loupe-lab/proto/`，沒有進 repo。
- **更新本機執行檔要用 `deploy-bin.sh`**：直接覆蓋執行中的執行檔，macOS 會因為簽章對不上把新行程砍掉。這次就發生過一次，服務停了幾分鐘，已恢復。
