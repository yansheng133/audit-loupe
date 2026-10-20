# 任務書：介面改版第 0 層——身分與資料量的事實調查（只讀）

寫作：2026-10-19；10/20 補 E–H 與 UI 新位置。這一輪**只查、不改**：不改程式、不改叢集、不改 Audit Loupe 設定。
結果是介面改版第 1 層（身分分類、後端篩選的 API 約定）的依據，所以每個結論都要有出處。

## 1. 現況（已確認，不用重查）

- lab：Rancher 2.15.2，所在叢集（Rancher 的 local）與下游叢集都是 RKE2 v1.36.4、arm64。
  - 下游叢集 ID `c-m-26kbxk5f`（Rancher 的叢集 ID，不是節點名稱），單一節點。
  - Audit Loupe 0.3.0 兩座都有裝（寫作時是 0.3.0，第一步實測）。
- 收尾狀態（取自 `docs/lab/report-sa-volume.md`，10/19 已核對）：
  - 下游 audit policy 已改回原本設定（sha256、stat、apiserver 參數都一致）。
  - `al-sample` 的範例 scale 到 0。
- 程式現況（我讀過程式碼）：
  - 「是不是人」的判斷在 `backend/src/audit.rs` 的 `is_human()`：名稱不是 `system:` 開頭就算人，`system:admin` 例外也算人。
  - 所以 `rke2-cloud-controller-manager` 這類不以 `system:` 開頭的元件身分會被當成人。這就是介面修正 3 的原因。
  - `is_rancher_user_id()`：`u-` 或 `user-` 開頭就當成 Rancher 使用者 ID。
  - 變更地圖（UI 已搬到另一個 repo：`~/audit-loupe-ui/pkg/audit-loupe/pages/ChangeMap.vue`，工作目錄裡有 10/20 未 commit 的版面修改，**不要動**）用 `/api/timeline?all=1&limit=2000` 一次取資料；後端上限 2000（`api.rs` 的 `clamp(1, 2000)`）。超過就被截斷，這是介面修正 2。
- lab 截圖（10/18 變更地圖）看到的兩個身分：
  - `rke2-cloud-controller-manager`：17:38 對 DaemonSet 做 deletecollection。
  - `u-d67owytc34`：沒有顯示名稱，改了 Node 29 次。

## 2. 不要碰

- 任何叢集物件、Helm release、Audit Loupe 的設定與資料庫：只讀。
- 規則 lab 留下的測試設定、`al-rules-*`、`al-sample`、`al-lab-*` 帳號：保持原狀，這一輪不清。
- `exp-*` namespace 與其他任務書的檔案。
- 不要開啟或印出 kubeconfig、`env.sh` 的內容；只用 `--kubeconfig <路徑>` 指定。

## 3. 我不確定的地方（請先推翻或證實）

1. **`u-d67owytc34` 是誰，我不知道。**
   - 我只從截圖看到它，不知道是哪座叢集的紀錄、經什麼方式進來。
   - 我的猜測（沒驗過）：可能是 Rancher 自己建的系統使用者，`principalIds` 裡有 `system://` 開頭的值，而不是真人。
   - 也可能是用 Rancher 發的 kubeconfig 直連的真人。
   - 請先到 local 叢集查 `users.management.cattle.io/u-d67owytc34`，**只取第 4 節列出的欄位**。
2. **「元件身分」的認法我沒查過。**
   - 我只看過名稱，沒看過這些紀錄的 `user.groups`、`user.extra`、User-Agent。
   - 不知道能不能用 groups（例如 `system:masters`、`system:nodes`）或憑證特徵來認，而不是一個個列名字。
3. **哪一天的資料完整，我不確定。**
   - 10/18 有收集中斷（台北 09:23–17:37），還有兩個 lab。
   - 我打算用 10/17（台北整天）當基準日，但沒確認 10/17 有沒有中斷、資料是否還在保留期內。請先用 `/api/coverage` 確認。

## 4. 目標（要回答的問題）

### A. `u-d67owytc34`
- 它是什麼：真人、Rancher 系統使用者，還是別的？
- 證據取自 local 叢集的 User 物件，**只取這些欄位**：
  - `metadata.name`、`displayName`、`username`、`principalIds`、`metadata.creationTimestamp`、`metadata.labels`、`metadata.annotations` 的鍵名（不要值）。
- **不可以取整個物件**：User 物件有 `password` 欄位（雜湊值），不能出現在終端機或報告裡。用 `-o jsonpath` 或 `-o json | jq` 只挑上面的欄位。
- 它綁了哪些 GlobalRoleBinding、ClusterRoleTemplateBinding（只列角色名稱）。
- 它在 Audit Loupe 的紀錄：
  - 在哪座叢集、經什麼方式（`via`：Rancher／impersonation／直連）。
  - 動作、資源、User-Agent 的分布。
  - 第一筆與最後一筆的時間。
- Audit Loupe 為什麼顯示不出名稱：`/api/users` 有沒有它？如果有，裡面記了什麼？

### B. 一天紀錄裡的身分組成（基準日見第 3 節第 3 點）
- 對兩座叢集，各列出基準日**所有不同的身分**（`all=1`，包含讀取）。
- 每個身分列出：
  - 名稱、筆數、寫入筆數
  - `user.groups`（取一筆代表）
  - `user.extra` 出現過的**鍵名**（不要值）
  - User-Agent 前 3 名
  - 目前程式判成人或非人
- 身分超過 60 個時，`system:serviceaccount:*` 依 namespace 合併成一列，其他照列。
- 每一列另外給一個**建議分類**：人、叢集元件、ServiceAccount、Rancher 內部、未知。**依據要寫出來**，例如「groups 含 system:nodes」。
  - 分類只是建議，最後由 Sam 決定。
  - 分不出來的放「未知」，不要硬分。

### C. 預設篩選要擋掉多少
- 基準日每座叢集的總筆數、寫入筆數。
- 依 B 的建議分類，各類各幾筆。
- 只留「人」與「未知」的寫入，一天還剩幾筆？2000 的上限夠不夠？

### D. 現有 API 的取數方式（讀程式即可，不用實測）
- 變更地圖、證據、規則、收集狀態四頁各打哪些 API、帶什麼參數、各自的筆數上限。
- 哪些篩選目前在後端做、哪些在前端做。

### E. 一天有多少紀錄（決定 5 秒的測試基準）
- 對兩座叢集，列出保留期內**每一天**的筆數。
- 每天分成：
  - events（`source=k8s`、`source=rancher` 分開）
  - versions
  - 找不到執行者的變更（`/api/unattributed`）
- 寫出哪一天最多。Sam 決定以「lab 實際量的 10 倍」當 5 秒的測試基準，所以這個數字要準。

### F. local 叢集的「Unknown」是怎麼來的
- 10/20 的變更地圖上，local 叢集有大量執行者是 Unknown 的變更，畫面寫「Collection gap」「Comparing versions after reconnecting」。例如：
  - `cattle-system` 裡的 `pod-impersonation-shell-*`
  - `liz-demo` 的 Deployment `web` 反覆建立、刪除
- 我的推測（**沒驗證**）：local 叢集沒有收 apiserver audit log，只收 Rancher 的操作紀錄，所以這些變更只有物件版本、沒有 audit 可以對；不是斷線。
- 請確認以下幾點，並用 `/api/coverage` 與 values 的現值當證據：
  - local 裝了哪幾種收集元件。
  - local 的 apiserver audit log 有沒有開。
  - 這些變更在 `gaps` 表裡有沒有對應的中斷。

### G. 節點被修改 45 次，為什麼看不到「改了什麼」
- 10/20 `u-d67owytc34` 經 Rancher 修改 Node `rke2.home.lab` 約 45 次，證據欄沒有差異內容。
- 請查：
  - 這些紀錄有沒有 request body。
  - 有沒有對應的物件版本。
  - 物件版本收集有沒有包含 Node。
  - audit policy 對 Node 記到哪個層級。

### H. 每支 API 單獨要多久（找出慢的那一支）
- 經 127.0.0.1 port-forward，對變更地圖用到的每支 API **一次只打一支**（不要同時打），各打 5 次，記錄每次的時間與回應大小。
  - 參數用 10/20 整天，和介面相同。
- 再把 6 支同時打 5 次，對照排隊造成的差距。
- 背景：後端只有一條資料庫連線（`store.rs` 第 18 行），同時送出的查詢會排隊；10/20 在瀏覽器量到 `/api/clusters`、`/api/stories`、`/api/actors`、`/api/timeline` 各約 3.1～3.5 秒。

### I. Rancher 自己的 audit log 怎麼設、怎麼輪替（唯讀）
- 從 Helm values（`helm get values`）與 Rancher Deployment 的容器定義，列出：`auditLog.level`、`destination`（sidecar 或 hostPath）、`maxAge`、`maxBackup`、`maxSize`，以及 rancher 容器實際拿到的環境變數或參數（只列名稱與值，不列任何 token）。
- sidecar 模式下，audit log 寫在哪個 volume（emptyDir 有沒有 sizeLimit）、目前目錄裡有哪些檔（`ls -li`，只看檔名、inode、大小、時間）。
- Audit Loupe 是怎麼收 Rancher audit log 的（哪個元件、讀哪個路徑），跟 apiserver audit log 是不是同一段程式。

### J. 經 Rancher 代理讀 Audit Loupe，會不會留下紀錄（唯讀）
- 背景：介面經 Rancher 的 service proxy 打 Audit Loupe API。設計上先假設會被 Rancher audit log 記下來，需要實際證據。
- 在 Rancher audit log 裡找 10/20 Sam 用變更地圖時的請求：requestURI 含 `services/` 與 `proxy` 的紀錄。
- 列出：筆數、verb、user 欄位的身分名稱、requestURI 的樣式（把 ID 換成 `<id>`）、有沒有記到 request／response body。**不要印出 header、cookie、token。**
- 找不到時，說明是 level 不記、還是 log 已經被輪替掉。

### K. 物件層級的量（v5 介面要用）
- 對基準日與 E 找到的最多那天：
  - 被動過的不同物件數（依叢集）、每個物件的操作次數分布（最大、中位數、前 10 名）。
  - 執行者不明的物件數。
  - 每小時、每 10 分鐘、每分鐘各有幾個時段有操作（決定時間列的粒度）。

## 5. 建議做法

1. 用 127.0.0.1 的 port-forward 連各叢集的 Audit Loupe server，打 `/api/coverage`、`/api/timeline?all=1&limit=2000`、`/api/actors`、`/api/users`。
   - 2000 筆不夠一天時，把一天切成小時段分批取。
2. `/api/timeline` 沒有 groups、extra 時：
   - 優先：用 sqlite 唯讀（`file:...?mode=ro`）讀 server Pod 裡的資料庫。
   - 次之：用 SA 量測錄到的原始 audit log（`~/audit-loupe-lab/recordings/2026-10-18/downstream/`）對照（只涵蓋下游、10/18 19:22–23:22）。
   - **不要為了取資料改程式或部署。**
3. 卡超過 30 分鐘的項目就停：寫下卡在哪、試過什麼，交回來由我們決定。不要為了湊齊數字耗掉整個下午。

## 6. 硬性限制

- 每一條 `kubectl`、`helm` 都明確寫 `--kubeconfig`。
- port-forward 只綁 127.0.0.1，用完關掉。
- 這一輪只讀：不 apply、不 patch、不 delete、不 helm upgrade、不重啟 Pod。`kubectl exec` 進 server Pod 也只能讀檔。
- 不把 token、密碼、密碼雜湊、cookie 寫進檔案或印到終端機。
- 報告不列 IP、不列 token。
- Mac 本機不用 sudo。

## 7. 驗證（每一條都要做，結果放報告）

1. **總數對得起來。**
   - B 裡每座叢集各身分的筆數加總，要等於同一時段 `/api/coverage`（或資料庫 `count(*)`）的總數。
   - 列出兩個數字。對不起來要說差多少、為什麼。
2. **沒有漏分類。**C 的各類加總要等於總數；「未知」那類要逐一列出名稱。
3. **跟原始 log 對一次。**
   - 取下游 10/18 19:22:38–23:22:38（UTC 11:22:38–15:22:38）這段。
   - 挑 3 個身分（一個人、一個元件、一個 SA）。
   - 比對 Audit Loupe 的筆數與 `~/audit-loupe-lab/recordings/2026-10-18/downstream/` 原始 log 的筆數。
   - 原始 log 計算規則照 `report-sa-volume.md` 回放那節：不重複 auditID、排除 watch、取 ResponseComplete／Panic，串流請求取 ResponseStarted。
   - 三組都要列，不接受「大致相符」。
4. **反向檢查。**被建議分成「人」的身分裡，不可以有 `system:serviceaccount:` 開頭的。有的話列出來並說明。
5. **E 的筆數對得起來。**任選一天，E 的 events 加總要等於該天 `/api/timeline?all=1` 分批取回的筆數，或資料庫 `count(*)`。
6. **H 的時間要有原始數字。**列出每一次的毫秒數，不接受只給平均。
7. **回歸。**結束時兩座叢集的 Audit Loupe server Pod 都要是同一個 Pod（重啟次數不變），`/api/integrity` 沒有新錯誤。這個用來證明這一輪沒有動到東西。

## 回報

- 報告寫在 `docs/lab/report-ui-facts.md`，開分支 `lab/ui-facts`、開 PR，不要自己合併。
- 報告內容：
  - A–K 的答案，每項附出處（API、資料庫查詢、程式碼行號）。
  - 第 7 節的逐項結果。
  - 我在第 3 節的假設哪些錯了。
  - **任務書哪裡讓你走冤枉路。**
  - 做了但任務書沒提到的任何動作。
- 不可以出現 token、密碼、密碼雜湊、IP。
