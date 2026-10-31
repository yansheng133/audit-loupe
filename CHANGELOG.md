# 變更紀錄

## 0.5.1（預覽版）

### 修正
- **server 在 lab 被記憶體上限砍掉（OOMKilled）**：檢查雜湊鏈時，SQLite 先把一座叢集的所有紀錄依 id 排序在暫存表裡。主連線的暫存放在記憶體，lab 約 50 萬筆時一次多用約 200 MB，再加上同時開的頁面就超過 512 MiB 上限。新增索引 `events_chain`（叢集＋id），紀錄依序讀出、不用排序：同一份 lab 資料，檢查時的記憶體高峰從 329 MB 降到 141 MB（閒置 140 MB）。0.3.0 起就有這個問題，lab 的 server 之前重啟過 6 次。
- **變更地圖的索引在 Kubernetes 上建不起來（`disk I/O error`）**：背景建索引和第二條讀取連線要寫暫存檔，但容器的根目錄是唯讀。server 多掛一個 emptyDir 在 `/tmp`（`server.tmpSize`，預設 1Gi）。0.5.0 在 lab 因此沒有用到 #22 的索引，變更地圖改讀整天（lab 的量還是很快）。
- 新增測試：雜湊鏈檢查的查詢計畫不能有暫存排序。

## 0.5.0（預覽版）

**預覽版**：第 1 層（變更地圖的資料底層）與身分表。CI 的效能閘門（每天約 72 萬筆）十個畫面 p95 都在 300 毫秒以內，單一物件細節 4 毫秒。介面另外在 [audit-loupe-ui](https://github.com/yansheng133/audit-loupe-ui) 發佈 0.5.0。升級注意事項見 `docs/deploy.md` 第 11 節「從 0.4.0 升級」。

### 新增
- **身分表與白名單**（#20）：每個身分（名稱加上 Rancher 使用者 ID）分類一次，分成人、叢集元件、ServiceAccount、Rancher 內部、未知；內建判斷認不出來的寫在 `server.identities`。規則只看「人」。新增 `GET /api/identities`。
- **變更地圖的時間範圍**（#22）：`from`／`to` 只看一段時間；同時開同一個畫面的人共用一次計算。
- **每個理由都有代碼**（#22）：介面依 Rancher 的語言顯示，不再只有中文原文。
- **效能閘門**（#21）：CI 產生一天約 72 萬筆紀錄，量十種畫面的時間，結果寫在 job summary。

### 變更
- **Rancher 叢集佈建帳號不再當成人**（#20）：principal 為 `system://` 的帳號（例如 `u-xxxxx`）歸「Rancher 內部」。lab 10/18–10/31 的資料重新判斷後，警示從 15,134 筆降到 3,089 筆，少掉的幾乎全是這個帳號與 `rke2-cloud-controller-manager` 的「非上班時間」；真人帳號的警示數不變。升級後要到「規則」頁執行一次重新計算，舊紀錄才會照新的分類判斷。
- **開頁只讀可能列出的紀錄**（#22）：新增覆蓋式部分索引 `events_shown`。第一次啟動時建立，20 萬筆以上在背景建，期間畫面註明「索引建立中」。
- **只計數、不列的請求**（#22）：Rancher 的取得目前使用者、搜尋、登出。刪除 namespace 後 controller 送的 `deletecollection` 併進那個 namespace 的列。
- **收集元件回報的缺口和心跳中斷分開顯示**（#22）；「開始收集」的標記不再當成缺口。
- **變更地圖預設列出 Rancher 內部帳號改到應用程式的物件**：Fleet 改應用程式的 Deployment、Service、Secret，Rancher cluster agent 在應用程式 namespace 建立或刪除 RoleBinding，這些是服務出問題時要先看的變更，以前和系統元件一起預設隱藏。現在只有改到 `kube-*`、`cattle-*`、`fleet-*`、`*-system` 這些 namespace 或叢集範圍物件的才隱藏。lab 10/20–10/30 的資料，每天多 0～4 列；叢集元件的寫入照舊隱藏。

### 修正
- **規則或白名單寫錯時，升級失敗而不是 server 停擺**：每次安裝或升級前，chart 先用新映像檔跑 `check-config`（Job `audit-loupe-config-check`）。讀不過就讓 `helm upgrade` 失敗，原本的 server 照常運作；以前 server 會先停掉、新的起不來，直到改好設定（lab 實測約停 1 分鐘）。可用 `server.configCheck.enabled: false` 關掉。

## 0.4.0（預覽版）

**預覽版**：v5 變更地圖與它的 API 第一次發佈。lab 的實際資料量下可以正常使用；「整頁 5 秒」的目標（每天約 72 萬筆）還沒有達到，見下方「已知限制」。介面另外在 [audit-loupe-ui](https://github.com/yansheng133/audit-loupe-ui) 發佈 0.4.0。

### 新增
- **變更地圖 API**：`GET /api/changemap`，一次請求取回整頁，包含總覽、各稽核狀態件數、時間列、關係、事件與預設選取物件的細節；`GET /api/changemap/detail` 只取一個物件的細節。約定見 `docs/api/changemap-v0.md`。
  - 稽核狀態：要看、看不清、看不到、清楚。
  - 時間列：預設每 10 分鐘一格；只選一個叢集時改成每分鐘一格。
  - 刪除整批（deletecollection）與由 server 命名的建立，列成一列「整批」。
  - 只是請求、沒有留下物件的（tokenreviews 這類）不列成物件，只計入數字。
  - 進入容器（exec、attach、port-forward）不論 audit log 記的動作是什麼，都顯示子資源名稱，並標出相關的 SUSE Security 設定。
- **收集元件回報「沒有收集」**：收集元件重新啟動、讀到一半的檔被刪、檔案被換掉時，會把沒有收集的起訖與原因送給 server。心跳中斷與收集元件回報的缺口都保留、都顯示。
- **讀不懂的行另外存**：audit log 裡損毀的行不再卡住整批，記下位置、原因與雜湊（不存內容），`GET /api/rejected` 查得到。
- **首次安裝的讀取方式**：`collector.file.onFirstStart`，預設 `fromNow`（從現在開始讀，不讀安裝前的舊紀錄）。改成 `readExisting` 的影響寫在 `values.yaml` 的註解。
- **重播工具**：`audit-loupe replay` 把保存的 audit log 重新送進一個 server，可平移時間、放大筆數，用來做實驗與量測。
- 讀取 API 在瀏覽器要求時回傳 gzip 壓縮；回應帶 `Server-Timing` 標頭。

### 改變
- **收集元件重新啟動時從現在開始讀**，停機期間的紀錄不補讀，改記成一段「沒有收集」（原則：不補資料）。
- 認檔不再只靠 inode，連同檔案開頭的雜湊一起比對。
- 變更地圖用到的「每座叢集最新紀錄時間」「資源的 kind」改由兩張小表提供，開頁時不再掃全部歷史。

### 升級（從 0.3.x）
- 資料庫在 server 第一次啟動時自動更新（缺口表加欄位、新增小表並回填一次）。
- 收集元件更新會重新啟動，**會留下一段「沒有收集」**，這是預期行為。
- values 沒有必須改的項目；新項目 `collector.file.onFirstStart` 只影響第一次安裝的節點。
- 舊的 8 支 API 照常提供。

### 已知限制
- **整頁 5 秒的目標還沒達到**：每天約 71 萬筆時，一次開頁伺服器端約 1 秒，同時有幾個人開頁會更慢。之後的版本改成收資料時就先彙總。
- 身分分類（人、叢集元件、ServiceAccount、Rancher 內部、未知）還不能自訂；Rancher 叢集佈建用的帳號仍會觸發「非上班時間」這類警示。
- server 保存紀錄的預設天數仍是 30 天。
- API 約定仍標為 v0，之後可能調整。
