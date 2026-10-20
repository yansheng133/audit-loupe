# 任務書：lab 收尾

寫作：2026-10-19。**接在 `docs/lab/ui-facts-task.md` 之後做。**那一輪的驗證要確認「server Pod 沒有被重啟」，而這一輪會 `helm upgrade` 重啟 server，順序不能反過來。

這一輪會改東西，每一項都要記下「做之前的狀態、做了什麼、做之後的狀態」。

## 1. 現況（已確認，不用重查）

前幾輪報告留下、還沒清的東西：

| 項目 | 出處 |
|---|---|
| server 規則是測試設定：`pod_exec` 改「高」並排除 `al-rules-a`，`secret_read` 關閉 | `docs/lab/report-rules.md`「收尾」 |
| namespace `al-rules-a`、`al-rules-b` 與其中只會 sleep 的 Pod | 同上 |
| m1-m2 留下的測試帳號 `al-lab-member`、`al-lab-noproxy`；Role／RoleBinding `audit-loupe-viewer` | 同上（m1-m2 的收尾沒做） |
| namespace `al-sample`（SA 量測範例，已 scale 到 0） | `docs/lab/report-sa-volume.md`「收尾」 |

- server 用 Helm 裝在 Rancher 所在叢集的 `audit-loupe-system`。
- Deployment 的更新方式是 `Recreate`：upgrade 時後端會停一下，收集元件會重送，不會掉資料（規則 lab 已驗證）。

## 2. 我不確定的地方（請第一步確認）

- **`al-rules-*` 在哪座叢集，我沒查。**報告只寫「建了兩個 namespace」。請兩座都看。
- **測試帳號是怎麼建的，我不知道。**可能是 Rancher 使用者（`users.management.cattle.io`），也可能另有 token。
  - 請先列出這兩個帳號在 Rancher 上的使用者物件、它們的 token、綁定。
  - 只列名稱與角色，**不要印出 User 物件的 `password` 欄位或任何 token 值**。
  - `audit-loupe-viewer` 在哪個 namespace、哪座叢集，也請一併確認。
- **目前 server 的 values 是什麼，以叢集上的現值為準**（`helm get values`），不要用 lab 資料夾裡的舊檔推測。

## 3. 要做的事

### A. 規則設定改回預設
1. 把 `server.rules` 改成 `{}`，其他 values 一個都不動，執行 `helm upgrade`。
2. 改之前與改之後各比一次 `helm get values`，差異只能是 `server.rules`。
3. 等 server 起來後，在規則頁會看到「較早的紀錄是用舊設定判斷的」提示。
4. **重新計算**：用 API `POST /api/rules/recompute`（經 service proxy，要帶 CSRF 標頭，做法見 `report-rules.md` 第 5 節）或在介面按「Judge again」。Sam 已同意這次重新計算。

### B. 刪掉規則 lab 的 namespace
- `al-rules-a`、`al-rules-b`。

### C. 保留 m1-m2 的測試帳號與權限（Sam 10/20 決定）
- `al-lab-member`、`al-lab-noproxy` 這兩個 Rancher 使用者，以及 Role／RoleBinding `audit-loupe-viewer`，**全部保留、不要動**。之後測權限覆核頁還要用。
- 只要在報告裡列出它們目前還在（名稱與綁定的角色名稱），不要印 token。

### D. 保留
- **`al-sample` 與裡面的定義保留，維持 scale 到 0。**之後還要用 pod-watcher 做 1 小時的 watch 量測。
- 錄下的 audit log（`~/audit-loupe-lab/recordings/`）保留。

### E. repo 裡的殘留檔
- `.git/stale-maintenance-lock-claude`：這是我在雲端那邊跑 `git fetch` 留下、改名後的空鎖檔，請刪掉。
- 刪之前先確認沒有正在跑的 git。

## 4. 硬性限制

- 每一條 `kubectl`、`helm` 都明確寫 `--kubeconfig`。
- port-forward 只綁 127.0.0.1，用完關掉。
- 除了第 3 節列的項目，什麼都不刪、不改。特別是：
  - `exp-*` namespace
  - laoban 與其他 extension
  - `al-sample`
  - 兩座叢集的 audit policy
- 不把 token、密碼、密碼雜湊印到終端機或寫進檔案。
- Mac 本機不用 sudo。
- 卡超過 30 分鐘的項目就停，寫下卡在哪，交回來。

## 5. 驗證（每一條都要做，結果放報告）

1. **規則真的回到預設。**
   - `GET /api/rules` 顯示的設定等於預設，設定紀錄多了一筆。
   - 用 API 查同一段固定期間（`report-rules.md` 附錄的 `from`／`to`）：
     - `secret_read` 警示回來了，筆數要等於規則 lab 量到的 127。
     - 警示總數等於 1,484。
   - 對不上就列差多少，不要寫「大致相符」。
2. **重新計算真的做完。**`/api/rules` 的 `pending` 是 0，重新計算紀錄的狀態是完成。
3. **刪掉的真的不見了。**對第 3 節 B 的每一項，用 `get` 確認 NotFound，逐項列出。
4. **反向檢查：不該刪的還在。**
   - `al-sample` 的 namespace 與 3 個 Deployment（replicas 0）還在。
   - 兩座叢集 audit-loupe 的 server／collector Pod 都在跑。
   - laoban extension 還在。
5. **資料沒掉。**
   - 收集狀態頁或 `/api/integrity` 的雜湊鏈驗證完整。
   - 兩座叢集在 upgrade 之後都有新紀錄進來（列出最後一筆的時間）。
6. **保留的帳號還在。**第 3 節 C 的兩個使用者與 `audit-loupe-viewer` 都還在，列出名稱與綁定的角色名稱（不印 token）。

## 回報

- 報告寫在 `docs/lab/report-cleanup.md`，和 `lab/ui-facts` 用同一個 PR，或另開分支 `lab/cleanup` 開 PR，不要自己合併。
- 報告內容：
  - 第 2 節的確認結果，以及我哪裡猜錯。
  - 每一項動作：之前、做了什麼、之後。
  - 第 5 節逐項結果。
  - 任務書沒提到但做了的事。
  - **任務書哪裡讓你走冤枉路。**
- 不可以出現 token、密碼、密碼雜湊、IP。
