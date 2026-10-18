# Lab 回報：ServiceAccount 讀取量量測

> **這是 lab 環境，範例流量是人工模擬的。**只有一個節點的下游叢集，上面除了 Rancher 自己的元件之外幾乎沒有真正的應用程式。數字可以拿來估「規則會多出多少紀錄」，但不能直接套到正式環境。

實測日期：2026-10-18，錄製時段台北時間 19:22:38–23:22:38（4 小時）。只量測與錄製，沒有改 Audit Loupe 的程式。

## 一句話結論

在下游 audit policy 加上「記錄 ServiceAccount 的 get／list」之後，這座 lab 叢集一天會多出約 **22 萬行、330 MB**。其中將近一半來自 Rancher 的 fleet-agent，約四成來自 local-path-provisioner，模擬的應用程式只占 4%。另外有兩件事要注意：

- **只靠 watch 運作的程式完全量不到。**
- **「沒有 pod-name」和「legacy token 標記」都不能單獨拿來認出叢集外的工具**，因為 Rancher 自己也用同一種 token。

## 名詞

| 名詞 | 意思 |
|---|---|
| ServiceAccount（SA） | 給程式用的帳號。Pod 裡的程式呼叫 Kubernetes API 時，用的就是它的 token |
| get／list／watch | 讀一個物件／列出一批物件／持續接收變動通知 |
| audit policy | 告訴 apiserver 哪些請求要寫進 audit log、寫多詳細 |
| Metadata 層級 | 只記誰、什麼時候、對什麼做了什麼，不記內容 |
| legacy token | 存在 Secret 裡、不會過期的舊式 SA token。新式 token 是 Pod 啟動時才發、會過期 |
| user.extra | audit log 裡附在使用者身上的額外資訊，例如這個 token 屬於哪個 Pod、哪個節點 |
| can-i | 程式問 apiserver「我能不能做某件事」的請求（`selfsubjectaccessreviews` 等） |
| 系統 namespace | 叢集本身元件所在的 namespace，清單見「扣掉系統 namespace 之後」 |
| 回放 | 把錄下來的 audit log 餵給本機的 Audit Loupe，確認它收到的筆數正確 |

## 環境

- Rancher 2.15.2。所在叢集與下游叢集都是 RKE2 v1.36.4（arm64），下游只有一個節點。
- 範例放在下游的 namespace `al-sample`（標籤 `audit-loupe.dev/lab=sample`）。每個範例有自己的 SA，只給最小權限。
  - 映像檔只用 SUSE BCI，腳本放在 ConfigMap 裡。
  - manifest 在 `docs/lab/sa-samples/`，裡面沒有任何 token。
- 錄到的 log 存在 Mac 的 lab 資料夾，沒有放進 repo。
  - 遮掉的只有 token、cookie、CSRF、credential-id 這類機密，IP 都保留。
  - 本報告只放統計數字，IP 一律不列。

## 範例實際做了什麼

| 範例 | 模擬對象 | 做什麼 | 4 小時內實際紀錄 |
|---|---|---|---|
| app-config | spring-cloud-kubernetes | 每 30 秒讀一個 ConfigMap 和一個 Secret | 478 輪，共 956 行（get configmaps 478、get secrets 478） |
| pod-watcher | kube-state-metrics | 啟動時 list 全叢集 Pod，之後用 watch 接著看（每段 5 分鐘），只有接不上時才重新 list | **0 行**（見發現 1） |
| ci-runner | Jenkins Kubernetes plugin | 每輪建一個短命 Pod（跑 10 秒），每 3 秒查一次狀態，讀 log，刪掉，再等 5 分鐘 | 46 輪：建立 46、刪除 46、get pods 244（每輪約 5 次）、讀 log 92 行（每輪 2 行，見發現 2） |
| external-tool | 叢集外的腳本 | 從 Mac 每 60 秒用 legacy token 列一次 `al-sample` 的 Pod | 240 次，全部回 200 |

external-tool 的 token 只留在 Mac 上的行程記憶體裡：從 Secret 讀進變數，用 `curl -H @-` 傳給 apiserver，不經過指令參數，也不寫檔。

## 量測步驟

1. **備份 audit policy：**備份下游的 `/etc/rancher/rke2/audit-policy.yaml`，記下 sha256、權限（600 root:root）、SELinux 標籤，以及 apiserver 的啟動參數。
2. **插入兩條規則，其他不動：**
   - 規則 A：放在第一條背景 None 規則之前。SA 的 `selfsubjectaccessreviews`、`selfsubjectrulesreviews` 記 Metadata。
   - 規則 B：放在 nonResourceURLs 的 None 規則之後。SA 的 get、list 記 Metadata。
3. **重啟 rke2-server：**apiserver 在 **42 秒**後恢復（`/readyz` 回 ok）。
4. **跑 4 小時：**範例照常執行，同時用 `kubectl get pods -A --watch-only` 計數 Pod 的新增與刪除，每 5 分鐘記一次 Pod 總數。
5. **收集 log：**收集下游 audit log，包括期間輪替出去的那一份。兩份檔案合起來剛好涵蓋整個時段，共 37,602 行。同時段的 Rancher audit log 也一起收。
6. **改回原本的 policy：**重啟後 apiserver 在 **43 秒**後恢復。sha256、權限、SELinux 標籤和 apiserver 參數**都跟原本一樣**。
7. **停掉範例：**範例 scale 到 0，external-tool 停掉。定義都留著。

## SA 讀取量

只算 SA 發出的 get／list（Metadata 層級，每一行是一筆 audit log）。

| 範圍 | 4 小時 | 換算一天 |
|---|---|---|
| **全部 SA 讀取** | 36,770 行、54.9 MB | **220,620 行、329.4 MB** |
| 範例（`al-sample`） | 1,532 行、2.0 MB | 9,192 行、12.3 MB |
| 系統 namespace | 2,942 行、4.7 MB | 17,652 行、28.2 MB |
| Rancher（`cattle-*`、`fleet-*`） | 17,872 行、27.1 MB | 107,232 行、162.9 MB |
| 其他（`local-path-storage`、`laoban-system`） | 14,424 行、21.0 MB | 86,544 行、126.0 MB |

- MB 是 10⁶ 位元組，以 audit log 原始一行的長度計算。
- 「換算一天」就是 4 小時 × 6，前提是一整天的流量都跟這 4 小時差不多。lab 叢集的請求大多是固定週期，這個假設大致成立；正式環境不一定。
- get 5,976 行、list 30,794 行。
- 依 SA 所在的 namespace 細分：

| namespace | 行數 |
|---|---|
| cattle-fleet-system | 15,990 |
| local-path-storage | 14,400 |
| kube-system | 2,926 |
| al-sample | 1,532 |
| cattle-impersonation-system | 1,438 |
| cattle-system | 444 |
| laoban-system | 24 |
| longhorn-system | 16 |

同時段的 **Rancher audit log** 共 2,764 行、3.3 MB，沒有任何一行是 SA 發出的。
- 依使用者：一個 Rancher 內部使用者 1,681 行、使用者名稱記成 `system:cattle:error` 的請求 976 行、管理者 107 行。
- 依方法：GET 2,533、PATCH 229、POST 2。

## 前 20 名（依 SA、動作、資源、User-Agent）

| # | SA | 動作 | 資源 | User-Agent | 行數 |
|---|---|---|---|---|---|
| 1 | cattle-fleet-system:fleet-agent | list | apps/deployments | fleetagent | 5,070 |
| 2 | cattle-fleet-system:fleet-agent | list | configmaps | fleetagent | 4,680 |
| 3 | cattle-fleet-system:fleet-agent | list | services | fleetagent | 4,680 |
| 4 | local-path-storage:local-path-provisioner | list | configmaps | local-path-provisioner | 2,880 |
| 5 | local-path-storage:local-path-provisioner | list | pods | local-path-provisioner | 2,880 |
| 6 | local-path-storage:local-path-provisioner | list | persistentvolumeclaims | local-path-provisioner | 2,880 |
| 7 | local-path-storage:local-path-provisioner | list | persistentvolumes | local-path-provisioner | 2,880 |
| 8 | local-path-storage:local-path-provisioner | list | storage.k8s.io/storageclasses | local-path-provisioner | 2,880 |
| 9 | kube-system:rke2-coredns-rke2-coredns-autoscaler | get | configmaps | cluster-proportional-autoscaler | 1,440 |
| 10 | kube-system:rke2-coredns-rke2-coredns-autoscaler | get | apps/deployments/scale | cluster-proportional-autoscaler | 1,440 |
| 11 | cattle-impersonation-system:cattle-impersonation-u-… | get | （API 根路徑 `/`） | cluster-api-controller-manager | 1,438 |
| 12 | al-sample:app-config | get | configmaps | app-config-sample | 478 |
| 13 | al-sample:app-config | get | secrets | app-config-sample | 478 |
| 14 | cattle-fleet-system:fleet-agent | list | networking.k8s.io/networkpolicies | fleetagent | 390 |
| 15 | cattle-fleet-system:fleet-agent | list | serviceaccounts | fleetagent | 390 |
| 16 | cattle-fleet-system:fleet-agent | list | rbac.authorization.k8s.io/clusterroles | fleetagent | 390 |
| 17 | cattle-fleet-system:fleet-agent | list | rbac.authorization.k8s.io/clusterrolebindings | fleetagent | 390 |
| 18 | cattle-system:cattle | get | namespaces | rancher | 271 |
| 19 | al-sample:ci-runner | get | pods | ci-runner-sample | 244 |
| 20 | al-sample:external-tool | list | pods | external-tool-sample | 240 |

前 3 名都是 fleet-agent，合計 14,430 行，占全部 SA 讀取的 39%。local-path-provisioner 每 5 秒把 5 種資源各列一次，合計 14,400 行，也占 39%。

## 扣掉系統 namespace 之後

**系統 namespace 清單：**kube-system、kube-public、kube-node-lease、default、calico-system、tigera-operator、cert-manager、longhorn-system、audit-loupe-system、cattle-neuvector-system、neuvector。

| 扣掉什麼 | 剩下（4 小時） | 換算一天 |
|---|---|---|
| 系統 namespace | 33,828 行、50.2 MB | 約 20.3 萬行、301 MB |
| 系統 namespace＋Rancher | 15,956 行、23.1 MB | 約 9.6 萬行、138 MB |
| 系統 namespace＋Rancher＋local-path-storage | 1,556 行 | 約 9,300 行 |

local-path-provisioner 不在原本的系統清單裡，但它是 RKE2 lab 常見的儲存元件。如果把它也算成系統元件，剩下的幾乎只有模擬範例（1,532 行）和一個 lab 自己的元件（24 行）。

## 不重複的組合

(SA, 動作, 資源, namespace, User-Agent, 來源 IP) 的不重複組合共 **36 個**，來源 IP 共 11 個（不列出）。換句話說，22 萬行一天的量其實是 36 種固定的請求在重複。

## user.extra 與 legacy token 標記

| 項目 | 結果 |
|---|---|
| user.extra 有 pod-name、node-name、credential-id | 34,820 行（三者一起出現或一起不出現） |
| 沒有這三項 | 1,950 行，分屬 3 個 SA：Rancher 代理用的 impersonation SA（1,438）、`cattle-system:cattle`（272）、external-tool（240） |
| external-tool 的請求帶 `authentication.k8s.io/legacy-token` | **240／240**，也都帶 `legacy-token-manual-secret` |
| 另外兩個 Rancher 的 SA 也帶這兩個標記 | 是 |

也就是說，**external-tool 的特徵（沒有 pod-name、帶 legacy token 標記）Rancher 自己的兩個 SA 也有**。要認出「叢集外拿 SA token 的工具」，還得加上 SA 名稱或來源 IP 來判斷。

## can-i 請求

規則 A 開著的 4 小時內，**SA 發出的 can-i 請求是 0 筆**。整份下游 log 裡任何使用者的 `selfsubjectaccessreviews`／`selfsubjectrulesreviews` 也都是 0。這座叢集上沒有元件會定期問權限，所以規則 A 在這裡沒有成本，也沒有測到東西。

## Pod 新增與刪除

| 項目 | 數字 |
|---|---|
| `--watch-only` 看到的新增 | 47 |
| `--watch-only` 看到的刪除 | 47 |
| audit log 裡的 Pod 建立／刪除（時段內） | 46／46，全部來自 ci-runner |
| Pod 總數（每 5 分鐘一次，共 49 次） | 81–82 |

- watch 計數器比錄製結束晚約 4 分鐘才停，多算到的那 1 次新增和刪除就是那段時間 ci-runner 跑的。
- 計數用的 watch 斷線重接了 6 次。重接期間發生的事件可能沒被看到，但新增和刪除都是 47，跟 audit log 對得上，所以這次沒有漏。

## 這次發現的事

1. **只用 watch 的程式量不到（中）。**
   - pod-watcher 在加規則之前就 list 過一次，之後一直用 watch 接著看，4 小時內一行都沒有。
   - 規則 B 只記 get／list，而像 kube-state-metrics 這種程式穩定運作時幾乎只發 watch。
   - 如果目的是「找出哪些 SA 在讀全叢集的 Pod 或 Secret」，光靠規則 B 會漏掉這一類。
2. **讀 Pod log 一次會寫兩行（低）。**
   - `pods/log` 是串流請求，audit log 會分別記「開始回應」和「回應結束」。
   - Audit Loupe 只保留其中一筆，所以 audit log 的行數會比 Audit Loupe 存的筆數多。這次剛好多 46 行。
   - 估容量時看 audit log 行數會稍微高估。
3. **大部分的量來自兩個元件（資訊）。**
   - fleet-agent 和 local-path-provisioner 合計占 78%。
   - 如果正式環境也有這兩個元件，在 policy 裡先把它們設成 None，量會掉一大截。
4. **重啟後 Rancher 的 Ready 不能當成恢復時間（低）。**
   - 兩次重啟，Rancher 顯示下游 Ready 的秒數都跟 apiserver 恢復的秒數一樣（42、43 秒）。
   - 看起來是 Rancher 還沒把狀態改成「未就緒」，所以並不是真的同時恢復。恢復時間以 apiserver 的 `/readyz` 為準。
5. **Rancher 代理的請求本身就是 SA 讀取（資訊）。**
   - 經 Rancher 轉送到下游的請求，在下游是以 impersonation SA 的身分出現。
   - 這次有 1,438 行，是 Rancher 端每 10 秒左右問一次下游的 API 根路徑。

## 回放

回放用的是本機 0.3.0 的 release 版執行檔。server 只聽 127.0.0.1，ingest 用隨機產生的測試 token，所有東西都放在暫存資料夾，結束後刪掉。

| 項目 | 數字 |
|---|---|
| 輸入 | 2 個檔案（輪替出去的那份在前），37,602 行，56.8 MB |
| 預期存入 | 37,556 筆 |
| 實際存入 | 37,556 筆，**一致** |
| 讀完所需時間 | 約 1 秒 |
| collect-file 與 server 的錯誤或警告 | 0 |

預期存入的算法：
- 不重複的 auditID。
- 動作不是 watch。
- 階段是 ResponseComplete 或 Panic；exec、attach、portforward 這種串流請求則用 ResponseStarted。

步驟：

```bash
REC=~/audit-loupe-lab/recordings/2026-10-18
W=$(mktemp -d)

# 1. 依時間順序接起來：輪替出去的檔案（audit-<時間>.log）在前，audit.log 在後
cat $(ls "$REC"/downstream/audit-*.log | sort) "$REC/downstream/audit.log" > "$W/all.log"

# 2. 起一個只聽 127.0.0.1 的 server，ingest token 用隨機值
umask 077; openssl rand -hex 16 > "$W/token"
printf 'replay %s\n' "$(cat "$W/token")" > "$W/tokens"
AL_DB_PATH="$W/db" AL_LISTEN=127.0.0.1:18180 AL_INGEST_LISTEN=127.0.0.1:18181 \
  AL_INGEST_TOKENS_FILE="$W/tokens" AL_CLUSTER=local AL_TZ=Asia/Taipei \
  backend/target/release/audit-loupe server &

# 3. 用 collect-file 餵進去，等 position.json 的 offset 等於檔案大小
AL_AUDIT_LOG="$W/all.log" AL_STATE_DIR="$W/state" AL_INGEST_URL=http://127.0.0.1:18181 \
  AL_TOKEN_FILE="$W/token" backend/target/release/audit-loupe collect-file &

# 4. 比對筆數
sqlite3 "file:$W/db?mode=ro" "SELECT count(*) FROM events WHERE cluster='replay'"
```

## 收尾

| 項目 | 狀態 |
|---|---|
| 下游 audit policy | 已改回原本的設定；sha256、權限、SELinux 標籤和 apiserver 參數都一致 |
| 範例 | 3 個 Deployment 都 scale 到 0，external-tool 已停；namespace 與定義保留 |
| Pod 計數程式 | 已停 |
| 錄到的 log | 在 Mac 的 lab 資料夾（權限 600），不進 repo |
| 回放用的暫存資料夾與測試 token | 已刪 |
| port-forward | 沒有使用 |

報告裡沒有任何 token、密碼、API key 或 IP。
