# 開發計畫：ServiceAccount 讀取偵測

寫作：2026-10-31。依據：`docs/lab/report-sa-volume.md`（10/18 量測）、Sam 10/18–10/19 的決定（下面「已定案」）。

**一句話**：把應用程式的 ServiceAccount（SA）讀取記下來，平常只存彙總；只有「SA 的 token 從不該出現的地方被拿來用」時，才存那一筆原始紀錄並發警示。

## 名詞

| 名詞 | 意思 |
|---|---|
| ServiceAccount（SA） | 給程式用的帳號。Pod 裡的程式呼叫 Kubernetes API 時用的就是它的 token |
| 彙總 | 同一個 SA、同一種動作、同一個資源、同一個來源，在一小時內的次數，以及第一次與最後一次的時間 |
| 來源 IP | audit log 記下的發出請求的位址 |
| legacy token | 存在 Secret 裡、不會過期的舊式 SA token |
| 系統 namespace | 叢集本身元件所在的 namespace（清單見第 2 節） |

## 1. 已定案（不再討論）

1. 範圍：應用程式 namespace 的 SA 的 get、list，記到 Metadata 層級（只記誰、何時、對什麼、做了什麼，不記內容）；系統 namespace 逐一排除。
2. 儲存：平常只存彙總；觸發規則的那一筆才存原始紀錄。
3. 「新行為」規則（某個 SA 第一次讀某種資源）這一版不做。
4. 「來源不對」：收集元件 watch Pod，只取名稱與 IP，不存版本；需要加 pods 的 list／watch 權限。
5. 來源比對：該 SA 的 Pod、節點、Helm 設定列出的 Rancher 伺服器 IP。Rancher 自己的 SA 不另外排除。
6. fleet-agent、local-path-provisioner 這類量大的元件，policy 設 None。代價：這些 SA 的 token 外洩時看不到，要寫進文件。
7. watch：先在 lab 量 1 小時（policy 也記 SA 的 watch），再決定要不要納入。→ 10/31 量完：預設不記，提供選用設定（第 4 節）。
8. 調校用的範例流量四種：應用程式讀設定、監控／控制器、CI runner、叢集外工具（舊式 token）。不做攻擊情境。

## 2. 設計

### 2.1 audit policy（`deploy/policy/rke2-audit-policy.yaml`）

在現有規則前面加三段：

1. **None**：量大的元件，用 `users` 逐一列出（`system:serviceaccount:cattle-fleet-system:fleet-agent`、`system:serviceaccount:local-path-storage:local-path-provisioner`）。
2. **None**：系統 namespace 的 SA，用 `userGroups: ["system:serviceaccounts:<namespace>"]` 逐一列出。audit policy 不支援萬用字元，所以清單寫死在檔案裡，部署文件說明怎麼增減。預設清單：kube-system、kube-public、kube-node-lease、cattle-system、cattle-fleet-system、cattle-impersonation-system、cattle-neuvector-system、longhorn-system、cert-manager、audit-loupe-system。
3. **Metadata**：其餘 SA（`userGroups: ["system:serviceaccounts"]`）的 get、list。
4. （選用，預設註解掉）**Metadata**＋`omitStages: ["ResponseComplete"]`：管理者列出的 namespace 的 SA 的 watch（第 4 節的決定）。

lab 估計：扣掉系統 namespace、Rancher 和 local-path-storage 之後，一天約 9,300 行；正式環境要另外估，文件寫估算方法（4 小時取樣、乘 6）。

### 2.2 收進來之後怎麼存

- server 收到一筆紀錄，如果是 SA 的 get／list（以及 watch，若納入），而且沒有觸發規則：**不寫進 events**，只加到彙總表：

  `sa_reads(cluster, actor, verb, api_group, resource, namespace, user_agent, source_ip, hour_ms, count, first_ms, last_ms)`，主鍵是前九欄。

- 觸發規則的那一筆照一般紀錄寫進 events（進雜湊鏈、可轉送 SIEM）。
- 彙總表不進雜湊鏈：它是統計，不是證據。介面與文件要明說。
- 保留天數與 events 相同（`server.retentionDays`）。
- lab 10/18 的量測：4 小時 36,770 行，不重複組合只有 36 個；換成每小時一列，一天約 900 列。

### 2.3 來源比對（規則 `sa_source`，預設嚴重度「中」）

- **watch 收集元件**多看兩種資源，只取需要的欄位，不存版本：
  - Pod：namespace、名稱、使用的 SA、IP、所在節點、開始與結束時間。
  - Node：名稱、InternalIP。
- server 存成 `pod_addrs(cluster, namespace, pod, sa, ip, node, from_ms, to_ms)` 與 `node_addrs(cluster, node, ip, from_ms, to_ms)`，保留天數同上。
- 判斷一筆 SA 請求的來源是不是「該在的地方」，下列任一成立就算正常：
  1. 當時有一個使用這個 SA 的 Pod，IP 就是來源 IP；
  2. 來源 IP 是叢集某個節點的 IP（hostNetwork 的 Pod、kubelet 代理）；
  3. 來源 IP 在 Helm 設定 `server.rancherServerIPs` 裡（Rancher 經 cluster agent 轉送的請求）。
- 都不成立就發警示，原始紀錄存進 events。token 是 bound token（user.extra 有 pod-name）時，警示寫出 token 原本屬於哪個 Pod；legacy token 寫「舊式 token，看不出原本屬於誰」。
- 收集元件剛啟動、還沒有 Pod 清單時：不判斷，記成「看不清」，不發警示（不能用推測補資料，Sam 10/20）。
- 規則與其他規則一樣可以關、改嚴重度、設例外（`server.rules.sa_source`）。

### 2.4 規則的「對象」欄位

規則設定多一個唯讀欄位「對象」：人或 SA（10/18 答應要預留，到現在還沒做）。現有規則都是「人」，`sa_source` 是「SA」。規則頁顯示，Helm 不能改。

### 2.5 介面

- 規則頁：多一條規則，顯示對象。
- 收集狀態頁：「SA 讀取」一節，每座叢集一天的彙總數字與前 10 名（SA、資源、次數），說明這些數字不在雜湊鏈裡。
- 變更地圖：`sa_source` 的警示和其他警示一樣出現在「要看」。版面先做模型給 Sam 看，定案後再寫程式（10/19 約定）。

## 3. 分階段

| 階段 | 內容 | 驗收 |
|---|---|---|
| S0 | lab 量 1 小時 watch（第 4 節） | **完成（10/31）**：預設不記，提供選用設定 |
| S1 | policy（2.1）＋文件 | `helm template`／policy 檔通過檢查；lab 套上後 apiserver 恢復時間記錄 |
| S2 | 彙總儲存（2.2） | 重播 10/18 錄下的 4 小時：events 不增加 SA 讀取、彙總表列數與次數和原始 log 一致 |
| S3 | Pod／Node 位址與 `sa_source`（2.3、2.4） | 重播：external-tool 240 次全部發警示、app-config／ci-runner／pod-watcher 0 次、Rancher 的 SA 0 次（要先在 values 列出 Rancher 伺服器 IP） |
| S4 | 介面（2.5） | 模型先給 Sam 看 |
| S5 | lab 實機 | 升級後 1 小時：external-tool 從 Mac 打的請求有警示、其他範例沒有；SA 讀取不影響變更地圖的效能閘門 |

S1–S3 一個 PR 一段，都附測試；S4 要等模型定案。

## 4. S0：watch 量測（1 小時）

- 做法：和 10/18 相同（`~/audit-loupe-lab/run/sa/policy.sh`），只是規則 B 多記 watch（`insert_rules_watch.py`），錄 1 小時，錄完改回原本的 policy。
- 要回答：
  1. SA 的 watch 一小時有幾行、多大；一天換算多少。
  2. pod-watcher（只用 watch 的範例）看不看得到。
  3. watch 的紀錄有沒有來源 IP 與 user.extra，能不能用同一個來源比對。
- 結果寫在本節下方。

### 結果（2026-10-31 10:24:53–11:25:00，台北）

做法照上面。policy 改回後 sha256、權限、SELinux 標籤、apiserver 參數都和原本相同；兩次重啟 apiserver 各停 43 秒。下游一小時 44,063 行在窗口內。統計用 `~/audit-loupe-lab/run/sa/analyze_watch.py`（IP 只計數、不列出）。

| 項目 | 一小時 | 換算一天 |
|---|---:|---:|
| SA 的 get | 2,344 行 | — |
| SA 的 list | 13,081 行 | — |
| **SA 的 watch** | **27,334 行、40.0 MB** | **約 65 萬行、958 MB** |

1. **watch 的量是 get＋list 的 1.8 倍。**每個 watch 寫兩行（`ResponseStarted` 13,778、`ResponseComplete` 13,556）。依 SA 所在 namespace：Rancher（`cattle-*`、`fleet-*`）15,524 行（57%，fleet-agent 一個就 11,564）；其他 8,321 行（suse-observability-agent 5,732、argocd 約 2,300、prometheus kube-state-metrics 397 等監控與控制器）；系統 namespace 3,465 行；範例 24 行。
2. **只用 watch 的程式記得到。**pod-watcher 一小時 24 行（每 5 分鐘一段 watch，開始與結束各一行）。10/18 只記 get／list 時是 0 行。
3. **watch 也能做來源比對。**27,334 行全部有來源 IP（不重複 32 個）；99.2%（27,116 行）的 user.extra 有 pod-name，沒有的 218 行。
4. 不重複組合（SA、資源、namespace、User-Agent、來源 IP）637 個，彙總儲存一樣划算。
5. 附帶：get＋list 一小時 15,425 行，比 10/18 的每小時 9,192 行多，因為 lab 後來多裝了 argocd、SUSE Observability agent、opencost、prometheus。正式環境的量要現場量，不能套 lab 數字。

### 決定（Sam 10/31 授權，由我決定）

- **watch 預設不記。**全叢集一天約 958 MB，多半是 Rancher 與監控元件；預設排除它們之後，剩下的仍主要是監控與控制器，不是要找的對象。
- **提供選用設定**：只對管理者列出的 namespace 記錄 SA 的 watch，而且只記開始（policy 的 `omitStages: ["ResponseComplete"]`），量減半，足以看出「這個 SA 在 watch 全叢集的 Pod」。來源比對（2.3）照樣適用。
- 2.1 的 policy 範本加這段選用規則（預設註解掉），部署文件寫估算方法。

## 5. 還沒決定、要 Sam 決定的

- 系統 namespace 預設清單（2.1）要不要加上 lab 以外常見的（calico-system、tigera-operator、gpu-operator 等）。
- `sa_source` 的預設嚴重度用「中」是否合適。
