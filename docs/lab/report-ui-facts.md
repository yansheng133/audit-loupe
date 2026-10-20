# 回報：介面改版第 0 層——身分與資料量的事實調查

執行：2026-10-20 23:15 至 10-21 00:08（台北）。對應任務書：`docs/lab/ui-facts-task.md`。全程只讀：沒有改程式、叢集或 Audit Loupe 的設定，兩座叢集的 Audit Loupe Pod 前後是同一批、重啟次數不變（第 7 節第 7 點）。

## 一句話結論

`u-d67owytc34` 不是人，是 Rancher 佈建下游叢集用的系統使用者（cluster-api 控制器），一天約 1,300 筆「改節點」其實全是只重排 finalizers 的空修改；一天的紀錄約 6.6 萬筆，真正由人做的寫入只有約 180 筆，所以 2,000 筆上限在「預設只看人」時夠用，在「看全部」時差 30 倍以上。

## 名詞

| 名詞 | 意思 |
|---|---|
| 身分 | 紀錄裡「誰做的」：Rancher 使用者、ServiceAccount、元件憑證等 |
| groups／extra | apiserver audit log 裡身分附帶的群組與額外欄位；Audit Loupe 的資料庫**沒有存**，只能從原始 log 取 |
| 基準日 | 用來量「一天」的日子。本報告用 **10/20（台北 00:00–24:00）** |
| 事後比對 | 物件版本收集元件看到物件變了，但沒有 audit 紀錄可以對到是誰做的 |
| SHOWN | 後端的篩選：經 Rancher 轉送、已經和下游紀錄配對的 Rancher 紀錄不重複顯示 |

## 資料來源與方法

- **資料庫**：從 server Pod 用 `kubectl exec … cat` 複製 `audit-loupe.db` 與 `-wal` 到 Mac（只讀檔，Pod 內沒有 sqlite），本機用 sqlite 唯讀分析。最後一次複製在 10-20T16:02:26Z（台北 10-21 00:02），`PRAGMA quick_check` 為 ok。
- **原始 audit log**：
  - 下游：collector Pod 唯讀掛載的 `/host-logs`。節點上目前有 3 個輪替檔與 `audit.log`，涵蓋 10-18T12:42Z 起；10/20 整天都在裡面。
  - 10/18 的比對用 SA 量測錄下的 `~/audit-loupe-lab/recordings/2026-10-18/downstream/`。
  - Rancher 自己的 audit log：rancher 容器裡 `/var/log/auditlog/rancher-api-audit.log`，用 `cat` 串給本機 Python 只取需要的欄位。**header、cookie、token 沒有印出、沒有存檔。**
- **API**：經 `127.0.0.1:18080` 的 port-forward 直接打 server 的 8080（不經 Rancher）。
- 分析腳本：`~/audit-loupe-lab/overnight/scripts/`（不進 repo）。

## 第 3 節的假設

| # | 假設 | 結果 |
|---|---|---|
| 1 | `u-d67owytc34` 可能是 Rancher 系統使用者，`principalIds` 有 `system://` | **對**。`principalIds` 是 `system://provisioning/fleet-default/rke2` 與 `local://u-d67owytc34` |
| 2 | 也許能用 groups 認元件 | **只對一半**。ServiceAccount 一定有 `system:serviceaccounts`；但 `rke2-cloud-controller-manager` 這種用憑證的元件，groups 只有 `system:authenticated`，跟一般用憑證的使用者一樣，**不能靠 groups 認**（見 B） |
| 3 | 用 10/17 台北整天當基準日 | **不行**。資料從 10/17 14:16 才開始（安裝時間），10/18、10/19 都有中斷；**唯一沒有中斷的整天是 10/20**，所以改用 10/20 |

## A. `u-d67owytc34` 是誰

**是 Rancher 的系統使用者，給 cluster-api 控制器佈建下游叢集用的，不是真人。**

| 欄位 | 值 | 出處 |
|---|---|---|
| `metadata.name` | `u-d67owytc34` | local 叢集 `users.management.cattle.io`，用 jq 只取這些欄位 |
| `displayName`、`username` | 都沒有（null） | 同上 |
| `principalIds` | `system://provisioning/fleet-default/rke2`、`local://u-d67owytc34` | 同上 |
| `creationTimestamp` | 2026-05-11T13:23:48Z（下游叢集建立的時候） | 同上 |
| labels | 一個鍵是雜湊、值為 `hashed-principal-name` 的 label | 同上 |
| annotation 鍵 | `lifecycle.cattle.io/create.mgmt-auth-users-controller` | 同上 |
| GlobalRoleBinding | 沒有 | `globalrolebindings` 依 `userName` 篩 |
| ClusterRoleTemplateBinding | `c-m-26kbxk5f-fleet-default-owner`：下游叢集的 `cluster-owner` | `clusterroletemplatebindings -A` |
| ProjectRoleTemplateBinding | 沒有 | 同上 |

Audit Loupe 裡的紀錄（保留期內全部）：

| 叢集 | 來源 | via | 筆數 | 第一筆～最後一筆（台北） |
|---|---|---|---|---|
| 下游 | apiserver（k8s） | rancher | 4,930 | 10/17 14:16 ～ 10/21 00:0x |
| 下游 | Rancher audit | rancher | 3,343 | 同上 |

- 動作：`patch nodes` 6,609 筆、`get`（Rancher 那份的非資源請求）1,598 筆，沒有別的。
- User-Agent 只有一種：`manager/unknown cluster-api-controller-manager (linux/arm64) …`。
- 經 Rancher 的扮演身分 `cattle-impersonation-system:cattle-impersonation-u-d67owytc34` 送到下游。
- **為什麼顯示不出名稱**：`/api/users`（資料庫 `users` 表）有它，但 `display_name` 是 `u-d67owytc34`、`username` 是空的。User 物件本身就沒有名稱，Audit Loupe 只能用 ID 代替。
- 目前程式把它判成「人」：`is_human()` 只看名稱是不是 `system:` 開頭（`backend/src/audit.rs:168`）。

## B. 一天紀錄裡的身分組成（基準日 10/20）

身分不到 60 個，全部列出。groups、extra、User-Agent 取自節點上的原始 log（下游）或 Rancher audit log（local）；Audit Loupe 資料庫沒有存 groups 與 extra。

### 下游（`c-m-26kbxk5f`）：apiserver 64,913 筆＋Rancher 轉送 1,321 筆

extra 欄的「pod 五鍵」指 `authentication.kubernetes.io/` 底下的 `credential-id`、`node-name`、`node-uid`、`pod-name`、`pod-uid`。

| 身分 | 筆數 | 寫入 | groups（代表一筆） | extra 鍵名 | User-Agent | 程式判定 | 建議分類 | 依據 |
|---|---|---|---|---|---|---|---|---|
| `system:serviceaccount:cattle-fleet-system:fleet-agent` | 27,744 | 0 | serviceaccounts、serviceaccounts:cattle-fleet-system、authenticated | pod 五鍵 | `fleetagent/v0.0.0` | 非人 | Rancher 內部 | SA 在 Rancher 的 namespace |
| `system:serviceaccount:local-path-storage:local-path-provisioner` | 17,280 | 0 | serviceaccounts、…:local-path-storage、authenticated | pod 五鍵 | `local-path-provisioner/v0.0.0` | 非人 | ServiceAccount | SA，一般工作負載 namespace |
| `system:serviceaccount:kube-system:rke2-coredns-rke2-coredns-autoscaler` | 8,640 | 0 | serviceaccounts、…:kube-system、authenticated | pod 五鍵 | `cluster-proportional-autoscaler/v0.0.0` | 非人 | 叢集元件 | SA 在 kube-system（RKE2 內建） |
| `system:serviceaccount:al-sample:app-config` | 5,742 | 0 | serviceaccounts、…:al-sample、authenticated | pod 五鍵 | `app-config-sample/1.0` | 非人 | ServiceAccount | SA |
| `system:serviceaccount:cattle-system:cattle` | 1,355 | 719 | serviceaccounts、…:cattle-system、authenticated | pod 五鍵 | `agent/v0.0.0`、`rancher/v0.0.0 … cluster c-m-…` | 非人 | Rancher 內部 | SA 在 cattle-system |
| `u-d67owytc34`（apiserver） | 1,321 | 1,321 | authenticated、cattle:authenticated | principalid、requesthost、requesttokenid | `cluster-api-controller-manager` | **人** | Rancher 內部 | User 物件的 principalIds 有 `system://`（A） |
| `u-d67owytc34`（Rancher 轉送） | 1,321 | 1,321 | 同上 | 同上 | 同上 | **人** | Rancher 內部 | 同上；這 1,321 筆都已和上一列配對，畫面不重複顯示 |
| `system:rke2-supervisor` | 683 | 463 | **masters**、authenticated | credential-id | `rke2-supervisor@…`、`helm-controller@…` | 非人 | 叢集元件 | 固定名稱、UA 為 RKE2 |
| `system:serviceaccount:kube-system:cilium` | 589 | 589 | serviceaccounts、…:kube-system、authenticated | pod 五鍵 | `cilium-agent/1.19.6` | 非人 | 叢集元件 | SA 在 kube-system |
| `system:serviceaccount:longhorn-system:longhorn-service-account` | 589 | 581 | serviceaccounts、…:longhorn-system、authenticated | pod 五鍵 | `longhorn-manager/v0.0.0` | 非人 | ServiceAccount | SA |
| `system:serviceaccount:al-sample:ci-runner` | 552 | 552 | serviceaccounts、…:al-sample、authenticated | pod 五鍵 | `ci-runner-sample/1.0` | 非人 | ServiceAccount | SA |
| `system:kube-scheduler` | 278 | 278 | authenticated | credential-id | `kube-scheduler/v1.36.4+rke2r1` | 非人 | 叢集元件 | 固定名稱 |
| `system:kube-controller-manager` | 65 | 65 | authenticated | credential-id | `kube-controller-manager/…` | 非人 | 叢集元件 | 固定名稱 |
| `system:serviceaccount:kube-system:cilium-operator` | 34 | 34 | serviceaccounts、…:kube-system、authenticated | pod 五鍵 | `cilium-operator-generic/1.19.6` | 非人 | 叢集元件 | SA 在 kube-system |
| `…kube-system:deployment-controller` | 11 | 11 | serviceaccounts、…:kube-system、authenticated | **只有 credential-id** | `kube-controller-manager/…` | 非人 | 叢集元件 | kube-system 的 SA 且 extra 沒有 pod 鍵＝controller-manager 內的控制器 |
| `…kube-system:replicaset-controller` | 8 | 8 | 同上 | 只有 credential-id | 同上 | 非人 | 叢集元件 | 同上 |
| `system:admin` | 5 | 3 | **masters**、authenticated | credential-id | `kubectl1.36.0 (darwin/arm64)`、`Helm/4.2.1` | **人** | 人 | 管理者 kubeconfig（憑證）。**實際是誰看不出**；這 5 筆是這次 CLI 在 Mac 上的唯讀查詢 |
| `…kube-system:cronjob-controller` | 5 | 5 | 同 deployment-controller | 只有 credential-id | `kube-controller-manager/…` | 非人 | 叢集元件 | 同上 |
| `…kube-system:job-controller` | 5 | 5 | 同上 | 只有 credential-id | 同上 | 非人 | 叢集元件 | 同上 |
| `…cattle-system:rancher-webhook` | 4 | 4 | serviceaccounts、…:cattle-system、authenticated | pod 五鍵 | `webhook/v0.0.0` | 非人 | Rancher 內部 | SA 在 cattle-system |
| `…cattle-neuvector-system:neuvector` | 1 | 1 | serviceaccounts、…:cattle-neuvector-system、authenticated | pod 五鍵 | `curl/8.14.1` | 非人 | Rancher 內部 | SA 在 `cattle-` namespace（NeuVector 是 Rancher 裝的；要不要另成一類請 Sam 決定） |
| `…kube-system:generic-garbage-collector` | 1 | 1 | 同 deployment-controller | 只有 credential-id | `kube-controller-manager/…` | 非人 | 叢集元件 | 同上 |
| `…kube-system:legacy-service-account-token-cleaner` | 1 | 0 | 同上 | 只有 credential-id | 同上 | 非人 | 叢集元件 | 同上 |

### local：只有 Rancher audit 307 筆（local 沒收 apiserver audit，見 F）

| 身分 | 筆數 | 寫入 | Rancher audit 的 `user` 欄 | 程式判定 | 建議分類 | 依據 |
|---|---|---|---|---|---|---|
| `admin`（`user-kgw7z`） | 207 | 175 | group：authenticated、cattle:authenticated；extra 鍵：principalid、requesthost、requesttokenid、username | 人 | 人 | Rancher 本機使用者（`local://`），UA 瀏覽器 |
| `system:cattle:error` | 100 | 100 | group：**unauthenticated**、cattle:error；沒有 extra | 非人 | 未知 | Rancher 未通過驗證的請求，看不出是誰 |

Rancher audit log 的 `user` 欄位只有 `name`、`group`、`extra` 三個鍵；Audit Loupe 只存 Rancher 的寫入、動作、登入與 Secret 讀取（`backend/src/rancher.rs:227` 的 `worth_keeping`），所以 local 一天只有 307 筆。

### 保留期內其他被判成「人」、但不是真人的身分（不在 10/20）

| 身分 | 筆數 | 日期 | 證據 | 建議分類 |
|---|---|---|---|---|
| `rke2-cloud-controller-manager` | 12 | 10/18–10/19 | groups 只有 `system:authenticated`、extra 只有 credential-id、UA `cloud-controller-manager/v1.36.3` | 叢集元件（**只能靠名稱或 UA 認**） |
| `laoban-cli-test`、`laoban-cli-test2` | 29、12 | 10/17–10/18 | UA `kubectl1.36.0 (darwin/arm64)`；原始 log 已輪替掉，查不到 groups | 未知（疑似 laoban 測試用憑證） |

## C. 預設篩選要擋掉多少（10/20）

| 叢集 | 總筆數 | 寫入 | 人 | 叢集元件 | ServiceAccount | Rancher 內部 | 未知 |
|---|---|---|---|---|---|---|---|
| 下游 | 66,234 | 5,961 | 5（寫入 3） | 10,320（寫入 1,459） | 24,163（寫入 1,133） | 31,746（寫入 3,366） | 0 |
| local | 307 | 275 | 207（寫入 175） | 0 | 0 | 0 | 100（寫入 100） |

- **只留「人」與「未知」的寫入，一天剩 278 筆**（下游 3＋local 275）。2,000 上限夠，而且有 7 倍餘裕。
- 但「看全部」（`all=1`）一天 66,541 筆，是上限的 33 倍；畫面只拿到**最後**的 2,000 筆（`ORDER BY ts_ms DESC LIMIT`，`store.rs:1770`），也就是當天最後約 45 分鐘。
- 判「寫入」的方法：apiserver 紀錄是 create／update／patch／delete／deletecollection 或 exec／attach／portforward；Rancher 紀錄除了 get、list（Secret 讀取）都算。

## D. 現有 API 的取數方式（讀程式）

| 頁面 | API 與參數 | 筆數上限 | 出處 |
|---|---|---|---|
| 變更地圖 | `/api/info` | — | UI `pages/ChangeMap.vue:226` |
| | `/api/timeline?from&to&all=1&limit=2000` | 2000（`clamp(1, 2000)`），取最新的 | `api.rs:181`、`store.rs:1770` |
| | `/api/unattributed?from&to` | 2000（SQL `LIMIT 2000`） | `store.rs:1532` |
| | `/api/actors?from&to` | 無 | `store.rs:1812` |
| | `/api/stories?from&to` | 無 | `api.rs:475` |
| | `/api/clusters?from&to` | 無 | `store.rs:1549` |
| | `/api/projects` | 無 | `api.rs:468` |
| | `/api/coverage?from&to` | 無 | `api.rs:292` |
| | 點物件時 `/api/related?cluster&kind&namespace&name` | 無 | `ChangeMap.vue:266` |
| | 規則提示列 `/api/rules` | — | `components/RecomputeBanner.vue:50` |
| 證據 | `/api/restore?version_id`、`/api/security` | — | `components/EvidencePanel.vue:50,71` |
| 規則 | `/api/rules`、`POST /api/rules/recompute` | — | `pages/Rules.vue:36` |
| 收集狀態 | `/api/coverage?hours=168`、`/api/integrity`、`/api/forwarding` | 無 | `pages/Coverage.vue:47-49` |

- **變更地圖一次開頁是 8 支 API，不是 6 支**（加上規則提示列共 9 個請求）。
- **後端做的篩選**：時間範圍、叢集、身分、「只看人」（`human=1`）、「只看寫入」、「只看警示」、隱藏已配對的 Rancher 紀錄（`SHOWN`）。變更地圖用 `all=1`，所以「只看人／只看寫入」都關掉了。
- **前端做的篩選**：叢集、專案、namespace、人員、系統元件開關、時間軸範圍、統計與分組，全部在瀏覽器裡對那 2,000 筆做（`lib/model.js` 的 `buildModel`）。
- 頁面要先等 Rancher 的叢集、專案清單（`findAll`）回來，才送出 8 支 API（`ChangeMap.vue:217-221`）。

## E. 一天有多少紀錄

保留期內的每一天（台北日期；資料從 10/17 14:16 開始，10/21 只有 2 分鐘）：

| 日期 | 叢集 | events（k8s） | events（rancher） | versions | 找不到執行者的變更 | 當天有無中斷 |
|---|---|---|---|---|---|---|
| 10/17 | 下游 | 21,928 | 429 | 740（其中 baseline 568） | 7 | 有（14:16 開始，14:22–16:41） |
| 10/17 | local | 0 | 310 | 1,074（baseline 871） | 199 | 有 |
| 10/18 | 下游 | **71,239** | 782 | 161 | 2 | 有（09:23–17:37） |
| 10/18 | local | 0 | 223 | 97 | 95 | 有 |
| 10/19 | 下游 | 54,355 | 991 | 82 | 0 | 有（07:52–09:24、16:54–18:18） |
| 10/19 | local | 0 | 184 | 70 | 69 | 有 |
| 10/20 | 下游 | 64,913 | 1,321 | 15 | 0 | **無** |
| 10/20 | local | 0 | 307 | 218 | 215 | **無** |

- **最多的一天是 10/18：兩座加起來 72,244 筆 events**（下游 72,021＋local 223）。而且那天還中斷了 8 小時多，又有 SA 量測（policy 暫時多記 ServiceAccount 讀取）與規則 lab，**不是平常的量**。
- **沒有中斷、也沒有 lab 的 10/20：66,541 筆 events**。
- 10 倍基準的建議：**以 10/18 的 72,244 筆為基準，取 10 倍 ≈ 72 萬筆／天**。理由是它是保留期內實測最大值；10/20 的 10 倍約 66.5 萬。最後由 Sam 決定。
- 「找不到執行者的變更」的算法和 `/api/unattributed` 相同：`versions` 裡 `event_id IS NULL AND change <> 'baseline'`。

## F. local 的「Unknown」怎麼來的

**推測正確：local 沒有收 apiserver audit log，那些變更只有物件版本、沒有 audit 可以對；不是斷線。畫面寫「Collection gap」「Comparing versions after reconnecting」是錯的說法。**

- local 裝的收集元件：`rancher`（Rancher audit）與 `watch`（物件版本），**沒有 `file`**。證據：`/api/coverage` 的 local 只有這兩個 collector；`helm get values` 的 local 只有 `collector.rancher.enabled` 與 `collector.watch.enabled`。
- local 的 apiserver audit log：**沒有開**。local 的 `kube-apiserver` Pod 參數裡沒有任何 `audit` 開頭的參數（下游有 `--audit-policy-file` 等 5 個）。
- `gaps` 表在 10/20 **沒有任何中斷**。10/20 local 有 215 筆找不到執行者的變更，例如：
  - `pod-impersonation-shell-*`：14 個 ClusterRole、14 個 ClusterRoleBinding、15 個 cattle-system 的 ServiceAccount，各自建立又刪除（Rancher 開 kubectl shell 時建的暫時權限）。
  - `liz-demo` 的 Deployment `web`：建立 10 次、刪除 9 次、修改 8 次。
- 畫面的字是寫死的：所有找不到執行者的變更，管道一律顯示 `unknown`＝「Collection gap／收集中斷」、發現方式一律是「Comparing versions after reconnecting／重新連線後比對物件版本」（UI `lib/model.js:143,191,507,540`），不看真正的原因。

## G. 節點被修改很多次，為什麼看不到「改了什麼」

- **有 request body，但內容是空修改。**10/20 `u-d67owytc34` 對下游節點的 `patch nodes` 有 1,290 筆 apiserver 紀錄，**1,290 筆都有 body**，而且全部只有 `metadata.$setElementOrder/finalizers`（加上 `resourceVersion`）：cluster-api 控制器在重排兩個 finalizer 的順序，節點實際沒有變。另外 1,290 筆是同一動作的 Rancher 紀錄，Rancher level 1 不記 body。
- **沒有物件版本。**`versions` 裡 Node 是 0 筆；物件版本收集清單（`/api/coverage` 的 watch details）沒有 `v1/nodes`。
- **audit policy 對 Node**：寫入落在 `level: Request`（「其他所有變更，含 request body」），所以 body 有記（`deploy/policy/rke2-audit-policy.yaml`）。
- **證據欄為什麼沒有差異**：證據欄的「改了什麼」來自物件版本的前後比對；Node 沒有版本，所以沒有差異可以顯示。
- 畫面看到「約 45 次」：當天有 1,290 次，但 `/api/timeline` 只回傳最新的 2,000 筆（約最後 45 分鐘），所以畫面只看得到其中一小段。
- 另外：`nodes/longhorn-system/rke2…` 那一列其實是 Longhorn 的 `nodes.longhorn.io`，物件鍵沒有分 API group，跟 Kubernetes 的 Node 名稱一樣（見 K 的前 10 名）。

## H. 每支 API 單獨要多久

經 `127.0.0.1` port-forward 直連 server（不經 Rancher）。參數 10/20 整天（`from=2026-10-19T16:00:00.000Z&to=2026-10-20T16:00:00.000Z`）。原始輸出：`~/audit-loupe-lab/overnight/h-timing.txt`。

一次只打一支，各 5 次（毫秒）：

| API | 第 1 次 | 第 2 次 | 第 3 次 | 第 4 次 | 第 5 次 | 回應大小 |
|---|---|---|---|---|---|---|
| `/api/info` | 18 | 3 | 3 | 3 | 3 | 140 B |
| `/api/timeline?all=1&limit=2000` | 53 | 13 | 12 | 13 | 13 | 1,431,808 B |
| `/api/unattributed` | 27 | 9 | 8 | 8 | 10 | 97,086 B |
| `/api/actors` | 457 | 365 | 359 | 362 | 372 | 6,094 B |
| `/api/stories` | 3 | 3 | 3 | 3 | 2 | 2 B（當天沒有） |
| `/api/clusters` | **3,375** | 284 | 263 | 267 | 286 | 329 B |
| `/api/projects` | 8 | 3 | 7 | 3 | 8 | 2,565 B |
| `/api/coverage` | 10 | 6 | 6 | 8 | 4 | 2,408 B |

8 支同時打，5 輪（毫秒）：

| API | 第 1 輪 | 第 2 輪 | 第 3 輪 | 第 4 輪 | 第 5 輪 |
|---|---|---|---|---|---|
| `/api/info` | 5 | 24 | 4 | 7 | 9 |
| `/api/timeline` | 40 | 50 | 20 | 23 | 34 |
| `/api/actors` | 461 | 698 | 382 | 685 | 393 |
| `/api/clusters` | 1,869 | 315 | 653 | 305 | 769 |
| `/api/projects` | 1,870 | 29 | 13 | 301 | 24 |
| `/api/stories` | 1,885 | 16 | 384 | 20 | 31 |
| `/api/unattributed` | 1,893 | 31 | 388 | 23 | 27 |
| `/api/coverage` | 1,898 | 12 | 12 | 17 | 30 |
| **整批（最慢一支）** | 1,950 | 753 | 703 | 745 | 828 |

- **慢的是 `/api/clusters`**：第一次（冷）3.4 秒，之後約 0.27 秒。它用 `SELECT DISTINCT cluster FROM events UNION …` 掃整張表，再對每個叢集數 events、versions（`store.rs:1549`）。其次是 `/api/actors` 約 0.36 秒（每個身分一個相關子查詢）。
- **排隊看得出來**：同時打時，跑得快的 API 會排在慢的後面（第 1 輪大家都等到約 1.9 秒；第 3 輪 stories、unattributed 被拖到 380 毫秒），這是單一連線加鎖的結果（`store.rs:18`）。
- **直連 server 比瀏覽器量到的快很多**：瀏覽器（經 Rancher 代理、開發伺服器）量到 4 支各約 3.1～3.5 秒，直連熱的時候整批不到 1 秒。差距可能來自：Rancher 代理、資料庫冷快取、當時同時有收集元件寫入。**這次沒有量經 Rancher 代理的時間，原因待查證。**
- 注意：我在量測前用 `cat` 複製過資料庫，可能讓節點的檔案快取變熱，第 2 次之後的數字偏樂觀。

## I. Rancher 自己的 audit log

| 項目 | 值 | 出處 |
|---|---|---|
| chart | `rancher-2.15.2`（revision 5） | `helm list -n cattle-system` |
| `auditLog.enabled` | true | `helm get values rancher`（只取 `auditLog`） |
| `auditLog.level` | 1 | 同上 |
| `auditLog.destination` | sidecar | 同上 |
| `maxAge`／`maxBackup`／`maxSize` | values 沒設（用 chart 預設） | 同上 |
| rancher 容器實際拿到的環境變數 | `AUDIT_LOG_ENABLED=true`、`AUDIT_LEVEL=1`、`AUDIT_LOG_MAXAGE=1`、`AUDIT_LOG_MAXBACKUP=1`、`AUDIT_LOG_MAXSIZE=100` | Deployment `rancher` 的容器定義 |
| sidecar | `rancher-audit-log`，`mirrored-bci-micro:16.0-15.11`，參數 `-F /var/log/auditlog/rancher-api-audit.log` | 同上 |
| volume | `audit-log`，**emptyDir，沒有 sizeLimit**，兩個容器都掛在 `/var/log/auditlog` | 同上 |
| 目前的檔 | 只有一個：`rancher-api-audit.log`，inode 17805515，81,745,079 bytes，10-20T15:27Z。**還沒有輪替檔** | `kubectl exec … -c rancher -- ls -li` |

- **sidecar 模式會不會照 `maxSize` 輪替：唯讀判斷不了。**檔案從 10-17T06:15Z 寫到現在約 78 MB（一天約 23 MB），還沒到 100 MB，所以看不到它會不會輪替。照這個速度約再 18 小時會到 100 MB。這留給第 4 項。
- **Audit Loupe 怎麼收**：local 的 `audit-loupe-rancher` Pod（`collect-rancher`）每 15 秒列出 Rancher Pod，用 Kubernetes 的 log API 跟著 sidecar 的輸出讀（`follow=true`、`timestamps=true`），位置記成「每個 Pod UID 最後一行的 kubelet 時間」（`collect.rs:295-430`）。**它不讀檔案，跟 apiserver 的 `collect-file`（讀節點上的檔、記 inode＋位置）是不同的程式。**所以 Rancher 那份 log 的輪替影響的是 sidecar 的 `tail -F`，以及 kubelet 對容器 log 的輪替，不是第 3 項要修的那段程式。
- 小發現：`/api/coverage` 的 local `rancher` collector 一直顯示 `sidecarMissing: true`，但它其實正在讀（`podsFollowing: 1`）。原因是 `warned` 只要警告過一次就不會再改回 false（`collect.rs:422`）；Rancher Pod 重啟時 sidecar 短暫沒在跑，就會一直掛著。

## J. 經 Rancher 代理讀 Audit Loupe，會不會留下紀錄

**會，每一個請求都會被 Rancher audit log 記下來；但 Audit Loupe 自己不存這些讀取。**

10/20（台北）在 Rancher audit log 裡，URI 含 `audit-loupe` 與 `/proxy/` 的請求共 **240 筆**，全部 200，全部是 `GET`，身分都是 `user-kgw7z`（admin）：

| 次數 | 經代理打的 API |
|---|---|
| 26 | `/api/related` |
| 24 | `/api/rules`、`/api/coverage` |
| 22 | `/api/unattributed`、`/api/info`、`/api/clusters`、`/api/stories`、`/api/timeline`、`/api/actors`、`/api/projects` |
| 4 | `/api/access`、`/api/access/diff` |
| 2 | `/api/forwarding`、`/api/integrity` |

- requestURI 的樣式：`/k8s/clusters/local/api/v1/namespaces/audit-loupe-system/services/http:audit-loupe:8080/proxy/api/<api>?<query>`；**query 參數（時間範圍）也完整記下來**。
- 有沒有 body：**沒有**（level 1 不記 request／response body）。
- 每筆紀錄的鍵：`auditID`、`method`、`remoteAddr`、`requestHeader`、`requestTimestamp`、`requestURI`、`responseCode`、`responseHeader`、`responseTimestamp`、`user`。**level 1 也會記 request／response header**；header 內容沒有看、沒有印。
- 換算：10/20 開了變更地圖 22 次，每次 9 個請求（8 支 API＋規則提示列），另外點物件 26 次。
- Audit Loupe 的資料庫沒有這些讀取：Rancher 紀錄只保留寫入、動作、登入、Secret 讀取（`rancher.rs:227-232`）。10/20 的 Rancher 紀錄裡，URI 含 `services/…/proxy` 的只有 16 筆 `create`（對 `rancher-ai-agent` 的 POST）。

## K. 物件層級的量（v5 介面要用）

「被動過」＝有寫入（定義同 C），加上找不到執行者的變更；已配對的 Rancher 紀錄不重複算。

| 日子 | 叢集 | 被動過的物件 | 操作次數 | 每個物件：最多／中位數 | 執行者不明的物件 | 有操作的時段：每小時／每 10 分鐘／每分鐘 |
|---|---|---|---|---|---|---|
| 10/20 | 下游 | 607 | 4,362 | 2,212／2 | 0 | 24/24、144/144、937/1,440 |
| 10/20 | local | 4 | 201 | 96／51 | 82（215 次） | 24/24、104/144、163/1,440 |
| 10/18 | 下游 | 593 | 3,257 | 1,057／2 | 2 | 17/24、96/144、559/1,440 |
| 10/18 | local | 10 | 158 | 65／4 | 49（95 次） | 17/24、72/144、108/1,440 |

10/20 下游前 10 名：Node 2,212（`u-d67owytc34` 1,290＋其他元件）、Longhorn 的 nodes 581、`ciliumidentities` 64、三個 Rancher 系統 namespace 各 50、kube-system 的兩個 SA 各 30、兩個 `clusterrepos` 24 與 22。10/20 local：`clusters/fleet-default/rke2` 96、`userpreferences` 86、rancher-ai-agent 的 service proxy 16、`liz-demo/web` 3。

- **時間列的粒度**：下游每 10 分鐘都有操作（144/144），每小時粒度會讓整條線一片滿；每分鐘也有 65% 的時段有操作。建議**每 10 分鐘一格**當預設，選了單一叢集再細到每分鐘。最後由 Sam 決定。
- 「次數最多」幾乎都是系統元件的例行動作，預設篩掉系統元件後，物件數會少很多。

## 第 7 節驗證

| # | 驗證 | 結果 |
|---|---|---|
| 1 | B 的各身分筆數加總＝資料庫 `count(*)` | 下游 66,234＝66,234；local 307＝307。另外下游 apiserver 的 64,913 筆，原始 log（10/20 整天、依 `to_record()` 的規則）也是 64,913，22 個身分逐一相同 |
| 2 | C 的各類加總＝總數，「未知」逐一列 | 下游 5＋10,320＋24,163＋31,746＋0＝66,234；local 207＋0＋0＋0＋100＝307。「未知」只有 `system:cattle:error`（local 100 筆） |
| 3 | 跟原始 log 對一次（下游 10/18 19:22:38–23:22:38） | 原始 log 37,556 筆＝資料庫 37,556 筆；**15 個身分逐一相同**。指定的三組：人（程式判定的）`u-d67owytc34` 1,651＝1,651；元件 `system:rke2-supervisor` 164＝164；SA `system:serviceaccount:al-sample:app-config` 956＝956。注意：這段時間**沒有真人**，程式判成人的只有 `u-d67owytc34`，A 已證明它不是人 |
| 4 | 被建議成「人」的不可以有 `system:serviceaccount:` | 沒有。建議成人的只有 `system:admin`（下游）與 `admin`（local） |
| 5 | E 的筆數對得起來 | 10/20 local：`/api/timeline?all=1&cluster=local` 307＝E 的 307。10/20 下游：分 96 段（每 15 分鐘）取回 64,913 筆，逐筆 id 與資料庫一致；E 的 66,234 減掉被 `SHOWN` 隱藏的 1,321 筆已配對 Rancher 紀錄＝64,913 |
| 6 | H 的時間要有原始數字 | 見 H 的兩張表 |
| 7 | 回歸：同一批 Pod、重啟次數不變、`/api/integrity` 沒有新錯誤 | 兩座叢集 5 個 Audit Loupe Pod 的名稱、UID、重啟次數前後相同（`diff` 為空）；`/api/integrity`：下游 verified 216,249、local 1,025，兩座 `first_bad_id` 都是 null |

## 任務書哪裡讓我走冤枉路

1. **基準日 10/17 不是整天**：資料從 10/17 14:16 才開始。改用 10/20，為此等到台北午夜才取完整的一天。
2. **「6 支 API」實際是 8 支**（加上規則提示列共 9 個請求）。H 照實際的 8 支量。
3. **groups、extra 不在 `/api/timeline` 也不在資料庫**。好在下游節點上的原始 log 還在（輪替保留約 3 天），10/20 整天都取得到；10/18 之前的身分（`laoban-cli-test`）已經輪替掉、查不到。
4. **任務書說 lab 是 0.3.0、夜間任務書說 v0.3.1**：實際是 **0.3.0**（`helm list` 與映像檔 tag）。
5. 我自己的錯：驗證 5 第一次分段時，把每段結尾寫成「下一段開頭減 1 毫秒再截到整秒」，每段漏掉最後一秒，少了 13 筆；改成精確到毫秒後一致。

## 做了但任務書沒提到的動作

- 把 server 的資料庫（含 `-wal`）與下游節點的兩個原始 audit log 複製到 Mac：`~/audit-loupe-lab/overnight/db/`、`~/audit-loupe-lab/overnight/raw/`（權限 600、不進 repo）。裡面有稽核資料，**Sam 看完報告後可以刪掉**。
- 讀了下游 collector Pod 的 `/state/position.json` 所在目錄的檔名（沒讀內容）。
- 為了唯讀取 groups，從 collector Pod 串流讀了節點上的 `audit-2026-10-19T07-56-36.821.log`，只取身分欄位，沒有存檔。
