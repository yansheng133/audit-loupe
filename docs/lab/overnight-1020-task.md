# 任務書：10/20 夜間批次（Sam 睡覺期間，無人可問）

寫作：2026-10-20。Sam 睡前已授權本文件列出的事，**沒列出的一律不做**。
執行期間沒有人可以回答問題。遇到不確定時，選比較保守的做法、寫進報告，然後繼續下一項；不要停在原地等。

## 0. 總原則

- **依序做**，每項有時間上限。超過上限就停在安全的狀態、寫下卡在哪，換下一項。
- **不合併任何 PR**，不發版（不打 tag、不推映像檔），不把開發版部署到 lab。
- 第 2 節原本的硬性限制全部照舊（`--kubeconfig`、port-forward 綁 127.0.0.1、不印 token／密碼／IP、Mac 不用 sudo、不碰 `exp-*`、不開 kubeconfig 與 `env.sh` 內容）。
- 只有第 4 項（輪替實測）與第 5 項（lab 收尾）可以改叢集，範圍以該項寫的為限。
- **出事時**：任何改叢集的步驟造成 lab 異常，且 30 分鐘內還原不了，就停掉所有會改叢集的項目（第 4、5 項），在總報告最上面寫「事故」一節：做了什麼、現在的狀態、建議 Sam 怎麼處理。不改叢集的項目可以繼續。
- 每項做完就 commit，不要全部做完才一次 commit，避免中途中斷什麼都沒留下。

## 1. 分支與 PR（整晚只開兩個 PR）

| PR | 分支 | 內容 |
|---|---|---|
| 甲 | `lab/overnight-1020`（從 main 開） | 文件與所有報告：需求文件、任務書、各項報告、總報告 |
| 乙 | `fix/collector-rotation`（從 main 開） | 收集元件的程式修正與測試 |

- 各任務書裡寫的「開分支 `lab/ui-facts`」「`lab/cleanup`」都改成放進甲，不另開。
- PR 描述最上面放總報告的連結與「需要 Sam 決定的事」。

## 2. 第 1 項：把未 commit 的東西收好（上限 20 分鐘）

1. **主 repo `~/audit-loupe`**：工作目錄裡未 commit 的文件（`docs/change-map-requirements.md`、`docs/lab/ui-facts-task.md`、`docs/lab/lab-cleanup-task.md`、本任務書）commit 到 `lab/overnight-1020`，push，開 PR 甲（先開 draft，最後再改成 ready）。
   - commit 前先 `git status` 列出所有變更，只收上面這些；有其他沒預期的變更就不要收，寫進報告。
2. **UI repo `~/audit-loupe-ui`**：工作目錄裡 10/20 的變更地圖版面修改（6 個檔，已被 v5 設計取代）commit 到新分支 `wip/change-map-layout-v4`，push，**不開 PR**。之後把工作目錄切回 `main` 並 pull。
   - commit 前列出變更檔，應該是：`ChangeMap.vue`、`FeedList.vue`（新檔）、`SidePanel.vue`、`TimeAxis.vue`、`lib/model.js`、`styles/change-map.scss`。多或少都寫進報告。
3. `.git/stale-maintenance-lock-claude` 留給第 5 項處理。

## 3. 第 2 項：第 0 層事實調查（上限 3 小時）

- 照 `docs/lab/ui-facts-task.md` 做 A–K，**全程只讀**。
- 這一項一定要在第 4、5 項之前做完，因為那兩項會改變資料。
- 報告：`docs/lab/report-ui-facts.md`，放進甲。

## 4. 第 3 項：修收集元件的四個缺陷（上限 3 小時）

**依據**：`docs/change-map-requirements.md` 第 3.6 節 (2) 的缺陷表與 (3) 的做法。以文件為準，本節只劃範圍。

### 範圍（只做這些）
1. **重新啟動時從現在開始讀，停機期間不補**（(3) 第 1 點）：啟動時跳到目前 `audit.log` 結尾；從上次讀到的最後一筆時間到這次啟動時間，送一筆「沒有收集」給 server，附原因與精確起訖。這同時消除缺陷 1。
2. **正在讀的檔被刪掉時**（缺陷 2）：立刻送「沒有收集」，起點是最後讀到的那筆時間，不要等心跳停 180 秒。
3. **落後量**（缺陷 3）：補讀輪替出去的檔期間、執行中發生輪替之後，`lagBytes` 都要正確（含尚未讀完的舊檔）。
4. **認檔不能只靠 inode**（缺陷 4，(3) 第 3 點）：位置連同檔案開頭一小段的雜湊一起記；inode 相同但開頭不同就當新檔，並送「沒有收集」。單一行損毀只隔離那一行並記錄（server 端記一筆、收集元件寫 WARN），整批其他行照常送，不能卡住。
5. server 端：接收收集元件送來的「沒有收集」紀錄（含原因、起訖、叢集、節點），存進既有的 `gaps` 或新表，`/api/coverage` 看得到。schema 有變就寫 migration。

### 不在範圍（寫進報告的「下一步」，不要做）
- 資料段表與 sha256（(3) 第 4 點）、遺失前警告（第 5 點）、後端停機時本機暫存（第 8 點）、進度回報（第 3.7 節）。
- 讀 `kube-apiserver.yaml` 取輪替參數。

### 測試
- 回放測試的參考腳本與當時的輸出在 `~/audit-loupe-lab/verify-collector/`（`scripts/`、`out/`）。腳本裡的路徑是雲端的，要改寫；**改寫成 repo 裡可以重複跑的測試**（Rust 整合測試，或 `scripts/` 下的回放腳本加上 CI 可以跑的入口，二選一，寫明理由）。
- 每個缺陷至少一個測試，先在 main 上跑出失敗（證明測試抓得到），修完再跑出通過。兩次結果都放報告。
- 既有測試全部要過；`cargo clippy` 不能有新警告。
- 情境至少包含：停機期間輪替兩次以上；讀到一半的檔被刪；inode 重複使用且位置落在行中間；單一行損毀；執行中輪替（剛輪替出去的檔要讀完，筆數等於原始行數）。

### 交付
- PR 乙。描述寫：每個缺陷怎麼修、行為改變了什麼（特別是「重啟不補讀」是新行為）、測試怎麼跑、`docs/deploy.md` 與 `docs/lab/report-rules.md` 哪些說法要跟著改（列出來，文件改動放在 PR 乙裡一起改）。
- 報告摘要放總報告，詳細寫在 PR 乙描述。

## 5. 第 4 項：在 lab 實測輪替（上限 2 小時，Sam 授權改設定，做完要還原）

目的：在真的叢集上看 v0.3.1（lab 目前的版本）遇到輪替時的行為，並確認 RKE2 的輪替參數能從節點上的 manifest 讀到。**lab 不換成修正版。**

### 下游 RKE2（`c-m-26kbxk5f`）
1. **改之前先存證**：節點上 `kube-apiserver.yaml` 的 sha256 與 audit 相關參數、叢集設定裡對應的值、Audit Loupe collector Pod 名稱與重啟次數、`/api/coverage` 現況。
2. 把 apiserver audit log 輪替調小：`audit-log-maxsize=1`（MB）、`audit-log-maxbackup=2`。改法照 SA 量測那次改 audit policy 的方式（見 `docs/lab/report-sa-volume.md`），經 Rancher 的叢集設定改，不要手改節點上的檔。
3. 用唯讀請求（例如反覆 `kubectl get`）產生 log，讓它輪替至少 5 次。期間記錄：檔名、inode、大小（`ls -li`）、collector 的 `lagBytes` 與 log、server 收到的筆數。
4. 刻意製造一次「收集元件停機期間輪替兩次以上」：
   - 先 `kubectl get ds -o yaml` 存下 collector DaemonSet 原本的 spec。
   - 暫時給它加一個不存在的 `nodeSelector`，讓 Pod 停掉；產生 log 讓它輪替兩次以上；再移除那個 `nodeSelector`，讓 Pod 回來。
   - 觀察缺陷 1、3、4 有沒有在真實環境出現（server 收到的筆數、`/api/coverage`、collector log）。
   - 還原後 DaemonSet 的 spec 要與存下的一致（diff 只能有 Kubernetes 自動維護的欄位）。
5. **還原**：參數改回原值；DaemonSet 已在上一步還原；確認 `kube-apiserver.yaml` 的 sha256 與改之前一致、audit 參數一致。collector 如果卡住（server 一直回 400、筆數不再增加），重建 collector Pod 讓它恢復，並記錄卡住的時段。
6. 報告裡寫明**哪一段時間的資料受這次測試影響**（之後看到那段的缺口或異常，才知道是測試造成的）。

### Rancher 自己的 audit log（sidecar）
- 先用第 2 項 I 的唯讀結果判斷。唯讀就能確定「sidecar 模式會不會照 maxSize 輪替」的話，**不改 Rancher**。
- 唯讀確定不了才改：`helm get values` 存下原值與目前 chart 版本，用**同一個 chart 版本**加 `--reuse-values` 只改 `auditLog.maxSize=1`，觀察 emptyDir 裡有沒有出現輪替檔，然後用存下的原值還原。
  - 上限 30 分鐘，`helm upgrade` 不要加 `--wait`，自己用 `kubectl rollout status` 等。
  - 還原後確認 Rancher Pod 都 Ready、`helm get values` 與原值一致（diff 為空）。
- 報告：`docs/lab/report-rotation-live.md`，放進甲。

## 6. 第 5 項：lab 收尾（上限 1 小時）

- 照 `docs/lab/lab-cleanup-task.md` 做（**已改成保留 `al-lab-*` 使用者與 `audit-loupe-viewer`**）。
- 報告：`docs/lab/report-cleanup.md`，放進甲。

## 7. 第 6 項：第 1 層後端的實作計畫（只寫計畫，不寫程式；上限 1.5 小時）

- 依 `docs/change-map-requirements.md` 第 3 節（5 秒呈現）與第 5 節（v5 頁面設計），加上第 2 項量到的數字，寫一份實作計畫給 Sam 審：
  - 物件的稽核狀態：判斷規則、在收資料時怎麼算、存哪裡、規則重新計算時怎麼更新。
  - 稽核狀態轉變事件。
  - 時間列彙總（依第 2 項 K 決定粒度）；預設一條合併線，選了叢集才分開（Sam 10/20 定案）。
  - 一次開頁一個請求的 API 長什麼樣子。
  - 讀寫分開（讀用連線池）、回應壓縮、伺服器端分頁與篩選。
  - 每一項：要改哪些檔、schema 變更與 migration、怎麼測、怎麼證明 5 秒內、預估工作量、風險。
  - 建議的 PR 切法與順序。
- 寫在 `docs/layer1-plan.md`，放進甲。**不要開始實作。**

## 8. 總報告

- `docs/lab/report-overnight-1020.md`，放進甲，最上面一張表：

| 項目 | 狀態（完成／部分／未做） | 產出（檔案或 PR） | 需要 Sam 決定的事 |
|---|---|---|---|

- 接著：
  - 事故（沒有就寫「無」）。
  - 對 lab 做過的所有變更，以及是否已還原（逐項）。
  - 各任務書哪裡讓你走冤枉路。
  - 做了但任務書沒提到的任何動作。
- 最後把 PR 甲從 draft 改成 ready。
